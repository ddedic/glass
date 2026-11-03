//! Trusts peers that Tailscale says belong to the same user as this Mac, so devices on
//! your own tailnet connect without a token.

use std::net::IpAddr;

use anyhow::{Context, Result};
use serde_json::Value;

/// `tailscale` on PATH, or the CLI inside the Mac App Store / standalone app bundle.
pub(crate) fn cli() -> &'static str {
    const BUNDLED: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    static CLI: std::sync::OnceLock<&str> = std::sync::OnceLock::new();
    CLI.get_or_init(|| {
        let on_path = std::process::Command::new("tailscale")
            .arg("version")
            .output()
            .is_ok_and(|o| o.status.success());
        if on_path || !std::path::Path::new(BUNDLED).exists() {
            "tailscale"
        } else {
            BUNDLED
        }
    })
}

async fn json(args: &[&str]) -> Result<Value> {
    let out = tokio::process::Command::new(cli())
        .args(args)
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "tailscale {}: {}",
        args[0],
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// This machine's Tailscale addresses (`100.x.x.x` and the tailnet IPv6), for `--bind tailscale`.
pub async fn self_ips() -> Result<Vec<IpAddr>> {
    let status = json(&["status", "--json"]).await.map_err(|e| {
        anyhow::anyhow!(
            "Tailscale isn't up, so there's no Tailscale address to listen on ({e:#}). Start it with `tailscale up`, or pass an IP address."
        )
    })?;
    self_addresses(&status)
}

/// `TailscaleIPs` from `tailscale status --json`, when the backend is running.
pub fn self_addresses(status: &Value) -> Result<Vec<IpAddr>> {
    let state = status["BackendState"].as_str().unwrap_or("unknown");
    if state != "Running" {
        anyhow::bail!("Tailscale isn't up ({state}). Run `tailscale up`, or pass an IP address.");
    }
    let Some(list) = status["Self"]["TailscaleIPs"].as_array() else {
        anyhow::bail!("Tailscale is running but reported no addresses.");
    };
    let mut ips = Vec::new();
    for value in list {
        let Some(text) = value.as_str() else { continue };
        let Ok(ip) = text.parse() else { continue };
        if !ips.contains(&ip) {
            ips.push(ip);
        }
    }
    anyhow::ensure!(
        !ips.is_empty(),
        "Tailscale is running but reported no addresses."
    );
    Ok(ips)
}

/// The login name of the tailnet user this Mac is signed in as.
pub async fn self_login() -> Result<String> {
    let status = json(&["status", "--json"]).await?;
    let uid = status["Self"]["UserID"]
        .as_u64()
        .context("no Self.UserID")?;
    status["User"][uid.to_string()]["LoginName"]
        .as_str()
        .map(str::to_owned)
        .context("no login name for self")
}

/// Whether `tailscale serve` already forwards HTTPS on 443 to Glass on `port` (how Linux
/// gets port-less URLs, since binding 443 there needs root).
pub async fn serves_https_to(port: u16) -> bool {
    json(&["serve", "status", "--json"])
        .await
        .is_ok_and(|v| serve_status_proxies_to(&v, port))
}

/// Makes `tailscale serve` forward https://<this machine>/ to Glass on `port`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub async fn serve_https_to(port: u16) -> Result<()> {
    let target = format!("https+insecure://localhost:{port}");
    let out = tokio::process::Command::new(cli())
        .args(["serve", "--bg", &target])
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "tailscale serve: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn serve_status_proxies_to(status: &Value, port: u16) -> bool {
    let target = format!("localhost:{port}");
    status["Web"].as_object().is_some_and(|web| {
        web.iter().any(|(host, conf)| {
            host.ends_with(":443")
                && conf["Handlers"]["/"]["Proxy"]
                    .as_str()
                    .is_some_and(|p| p.contains(&target))
        })
    })
}

/// A device on the tailnet that belongs to this Mac's owner.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Machine {
    pub name: String,
    /// MagicDNS name, e.g. `macbook.tail1234.ts.net` (no trailing dot).
    pub dns: String,
    pub os: String,
    pub online: bool,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// This device plus every peer owned by the same user.
pub async fn machines() -> Result<Vec<Machine>> {
    let status = json(&["status", "--json"]).await?;
    let me = status["Self"]["UserID"].as_u64();
    let machine = |node: &Value, is_self: bool| Machine {
        name: node["HostName"].as_str().unwrap_or_default().to_owned(),
        dns: node["DNSName"]
            .as_str()
            .unwrap_or_default()
            .trim_end_matches('.')
            .to_owned(),
        os: node["OS"].as_str().unwrap_or_default().to_owned(),
        online: is_self || node["Online"].as_bool().unwrap_or(false),
        is_self,
    };
    let mut out = vec![machine(&status["Self"], true)];
    if let Some(peers) = status["Peer"].as_object() {
        out.extend(
            peers
                .values()
                .filter(|p| p["UserID"].as_u64() == me)
                .map(|p| machine(p, false)),
        );
    }
    // Phones and tablets can view but can't run the host.
    out.retain(|m| !m.dns.is_empty() && !matches!(m.os.as_str(), "iOS" | "android"));
    Ok(out)
}

pub enum Probe {
    Missing,
    /// Installed, but the app (macOS) or tailscaled (Linux) isn't running.
    NotRunning,
    LoggedOut,
    /// Connected, but the tailnet hasn't enabled HTTPS certificates.
    NoHttps {
        name: String,
    },
    Ready {
        name: String,
    },
}

/// Where this Mac stands with Tailscale, for `glass setup`.
pub async fn probe() -> Probe {
    let Ok(status) = json(&["status", "--json"]).await else {
        let installed = std::process::Command::new(cli())
            .arg("version")
            .output()
            .is_ok();
        return if installed {
            Probe::NotRunning
        } else {
            Probe::Missing
        };
    };
    if status["BackendState"].as_str() != Some("Running") {
        return Probe::LoggedOut;
    }
    let dns = status["Self"]["DNSName"]
        .as_str()
        .unwrap_or_default()
        .trim_end_matches('.')
        .to_owned();
    match status["CertDomains"]
        .as_array()
        .and_then(|d| d.first())
        .and_then(|d| d.as_str())
    {
        Some(name) => Probe::Ready {
            name: name.to_owned(),
        },
        None => Probe::NoHttps { name: dns },
    }
}

/// Gets (or renews) a Let's Encrypt cert for this Mac's tailnet name via `tailscale cert`.
/// Returns None when Tailscale isn't running or the tailnet hasn't enabled HTTPS.
pub async fn ensure_cert(
    dir: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let status = json(&["status", "--json"]).await.ok()?;
    let name = status["CertDomains"]
        .as_array()?
        .first()?
        .as_str()?
        .to_owned();
    std::fs::create_dir_all(dir).ok()?;
    let cert = dir.join(format!("{name}.crt"));
    let key = dir.join(format!("{name}.key"));
    // `tailscale cert` only contacts Let's Encrypt when the cert is missing or near expiry.
    let out = tokio::process::Command::new(cli())
        .arg("cert")
        .arg("--cert-file")
        .arg(&cert)
        .arg("--key-file")
        .arg(&key)
        .arg(&name)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        tracing::warn!(
            "tailscale cert: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return None;
    }
    tracing::info!("using Tailscale HTTPS certificate for {name}");
    Some((cert, key))
}

/// `100.64.0.0/10` or `fd7a:115c:a1e0::/48`. Callers pass `IpAddr::to_canonical()` when
/// the address might be an IPv4-mapped IPv6 form.
pub(crate) fn is_tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        // 100.64.0.0/10 (CGNAT range Tailscale assigns from)
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64,
        // fd7a:115c:a1e0::/48
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// Who a tailnet device belongs to: its user's login name, or `None` for a device that
/// isn't anyone's (tagged, e.g. a server), or one off the tailnet.
pub async fn identity(peer: IpAddr) -> Option<String> {
    // Tests: every viewer is this tailnet user (GLASS_TEST_TAILNET_LOGIN).
    if crate::testmode::enabled() {
        return std::env::var("GLASS_TEST_TAILNET_LOGIN").ok();
    }
    let peer = peer.to_canonical();
    if !is_tailscale_ip(peer) {
        return None;
    }
    match json(&["whois", "--json", &peer.to_string()]).await {
        Ok(who) => login_of(&who),
        Err(e) => {
            tracing::warn!("tailscale whois {peer}: {e:#}");
            None
        }
    }
}

/// A whois answer's person: tagged devices have none ("tagged-devices").
fn login_of(who: &Value) -> Option<String> {
    let tagged = who["Node"]["Tags"].as_array().is_some_and(|t| !t.is_empty());
    let login = who["UserProfile"]["LoginName"].as_str()?;
    (!tagged && login != "tagged-devices" && !login.is_empty()).then(|| login.to_string())
}

/// What Tailscale knows about a tailnet peer, for device posture: `None` off the tailnet.
pub async fn whois(peer: IpAddr) -> Option<Value> {
    let peer = peer.to_canonical();
    if !is_tailscale_ip(peer) {
        return None;
    }
    json(&["whois", "--json", &peer.to_string()]).await.ok()
}

/// A tailnet device's name (e.g. "nathans-ipad"), for showing who's connected.
pub async fn device_name(peer: IpAddr) -> Option<String> {
    let who = json(&["whois", "--json", &peer.to_canonical().to_string()])
        .await
        .ok()?;
    let name = who["Node"]["ComputedName"]
        .as_str()
        .or_else(|| who["Node"]["Hostinfo"]["Hostname"].as_str())?;
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_owns_a_device_a_tag_does_not() {
        let who = |v: &str| login_of(&serde_json::from_str(v).unwrap());
        assert_eq!(who(r#"{"UserProfile":{"LoginName":"ana@example.com"},"Node":{}}"#), Some("ana@example.com".into()));
        assert_eq!(who(r#"{"UserProfile":{"LoginName":"ana@example.com"},"Node":{"Tags":["tag:server"]}}"#), None);
        assert_eq!(who(r#"{"UserProfile":{"LoginName":"tagged-devices"},"Node":{}}"#), None);
        assert_eq!(who(r#"{"Node":{}}"#), None);
    }

    #[test]
    fn tailscale_ranges() {
        assert!(is_tailscale_ip("100.101.102.103".parse().unwrap()));
        assert!(is_tailscale_ip(
            "fd7a:115c:a1e0::c138:8278".parse().unwrap()
        ));
        assert!(!is_tailscale_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale_ip("192.168.1.20".parse().unwrap()));
        assert!(is_tailscale_ip(
            "::ffff:100.64.0.7"
                .parse::<IpAddr>()
                .unwrap()
                .to_canonical()
        ));
    }

    #[test]
    fn reads_self_addresses() {
        let status = serde_json::json!({
            "BackendState": "Running",
            "Self": { "TailscaleIPs": ["100.64.0.2", "fd7a:115c:a1e0::12", "100.64.0.2"] }
        });
        assert_eq!(
            self_addresses(&status).unwrap(),
            vec![
                "100.64.0.2".parse::<IpAddr>().unwrap(),
                "fd7a:115c:a1e0::12".parse().unwrap(),
            ]
        );
        let logged_out = serde_json::json!({"BackendState": "NeedsLogin"});
        let err = self_addresses(&logged_out).unwrap_err().to_string();
        assert!(err.contains("NeedsLogin"), "{err}");
        let empty = serde_json::json!({"BackendState": "Running", "Self": {"TailscaleIPs": []}});
        assert!(self_addresses(&empty).is_err());
    }

    #[test]
    fn reads_serve_status() {
        let on: Value = serde_json::from_str(
            r#"{"TCP":{"443":{"HTTPS":true}},"Web":{"box.tail1.ts.net:443":{"Handlers":{"/":{"Proxy":"https+insecure://localhost:8080"}}}}}"#,
        )
        .unwrap();
        assert!(serve_status_proxies_to(&on, 8080));
        assert!(!serve_status_proxies_to(&on, 9090));
        assert!(!serve_status_proxies_to(&serde_json::json!({}), 8080));
    }
}
