//! Trusts peers that Tailscale says belong to the same user as this Mac, so devices on
//! your own tailnet connect without a token.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;

/// How long to wait between checks once Tailscale has given a definite answer.
const RECHECK: Duration = Duration::from_secs(30);
/// A connection-time lookup gives up quickly, so a flood of sign-ins can't stall on the CLI.
const LAZY_TIMEOUT: Duration = Duration::from_secs(1);
/// At most one of those lookups per interval, however many connections arrive.
const LAZY_GAP: Duration = Duration::from_secs(1);
/// The background check can wait longer; a hung CLI must not stall the loop forever.
const POLL_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Who this node belongs to, from one read of `tailscale status`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeOwner {
    /// A person's login. Their own devices may skip the token.
    User(String),
    /// Tagged, signed out, or the synthetic tagged-devices user. Not an owner.
    Nobody,
    /// The local API didn't answer. Not a change of who owns the node.
    Unavailable(String),
}

/// `tailscale status --json` → who owns this node.
///
/// `Running` is required before trusting a login: while Tailscale is still starting, `Self`
/// can describe the previous account. A tag wins over that login, so a tagged node never
/// gains an owner.
fn owner_from_status(status: &Value) -> NodeOwner {
    if status["Self"]["Tags"]
        .as_array()
        .is_some_and(|tags| !tags.is_empty())
    {
        return NodeOwner::Nobody;
    }
    match status["BackendState"].as_str() {
        Some("NeedsLogin") => return NodeOwner::Nobody,
        Some("Running") => {}
        other => {
            return NodeOwner::Unavailable(other.unwrap_or("no BackendState").to_string());
        }
    }
    let Some(uid) = status["Self"]["UserID"].as_u64() else {
        return NodeOwner::Unavailable("no Self.UserID".into());
    };
    match status["User"][uid.to_string()]["LoginName"].as_str() {
        Some(login) if !login.is_empty() && login != "tagged-devices" => {
            NodeOwner::User(login.to_owned())
        }
        Some(_) => NodeOwner::Nobody,
        None => NodeOwner::Unavailable("no login name for self".into()),
    }
}

async fn read_node_owner() -> NodeOwner {
    match json(&["status", "--json"]).await {
        Ok(status) => owner_from_status(&status),
        Err(e) => NodeOwner::Unavailable(format!("{e:#}")),
    }
}

async fn read_node_owner_within(limit: Duration) -> NodeOwner {
    match tokio::time::timeout(limit, read_node_owner()).await {
        Ok(found) => found,
        Err(_) => NodeOwner::Unavailable("timed out".into()),
    }
}

/// 1s, 2s, 4s, … capped at [`RECHECK`].
fn backoff(misses: u32) -> Duration {
    Duration::from_secs((1u64 << misses.min(5)).min(RECHECK.as_secs()))
}

/// What one refresh decided: how long to wait, and whether to say so in the log.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Refresh {
    wait: Duration,
    /// Login announced the first time it becomes the owner. Repeating it is silent.
    announce: Option<String>,
    /// The first time Tailscale doesn't answer while the owner is still unknown.
    unavailable: bool,
}

#[derive(Clone)]
enum OwnerState {
    /// Not yet a definite answer. Token required; keep asking.
    Unknown,
    User(String),
    /// Tagged or signed out. Token required, and it is not "still starting".
    Nobody,
}

struct OwnerInner {
    state: RwLock<OwnerState>,
    misses: Mutex<u32>,
    /// Last login we logged, so a retry or a blip doesn't log it again.
    announced: Mutex<Option<String>>,
    logged_unavailable: AtomicBool,
    last_lazy: Mutex<Option<Instant>>,
    /// One `tailscale status` at a time, shared by the background loop and connections.
    in_flight: AtomicBool,
}

/// Clears [`OwnerInner::in_flight`] when the lookup finishes, including on cancel.
struct Claim<'a> {
    flag: &'a AtomicBool,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// This computer's tailnet owner, resolved after startup and refreshed.
///
/// `get` is always the latest answer. `None` means unknown, tagged, or signed out: every
/// client needs a token, the same as when Tailscale was down at startup. Readers must use
/// this value, not a copy taken when the process started.
#[derive(Clone)]
pub struct TailnetOwner {
    inner: Arc<OwnerInner>,
}

impl TailnetOwner {
    fn new(state: OwnerState) -> Self {
        Self {
            inner: Arc::new(OwnerInner {
                state: RwLock::new(state),
                misses: Mutex::new(0),
                announced: Mutex::new(None),
                logged_unavailable: AtomicBool::new(false),
                last_lazy: Mutex::new(None),
                in_flight: AtomicBool::new(false),
            }),
        }
    }

    /// Claims the single in-flight lookup, or `None` when one is already running.
    fn claim(&self) -> Option<Claim<'_>> {
        self.inner
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Claim {
                flag: &self.inner.in_flight,
            })
    }

    /// No answer yet. The background loop and connection attempts fill it in.
    pub fn unknown() -> Self {
        Self::new(OwnerState::Unknown)
    }

    /// A fixed owner for `--test` (`GLASS_TEST_TAILNET_OWNER`). Never refreshed.
    pub fn fixed(login: Option<String>) -> Self {
        let state = match login {
            Some(login) => OwnerState::User(login),
            None => OwnerState::Nobody,
        };
        Self::new(state)
    }

    /// The login whose devices skip the token, or `None` when a token is required.
    pub fn get(&self) -> Option<String> {
        match &*self.inner.state.read().unwrap() {
            OwnerState::User(login) => Some(login.clone()),
            _ => None,
        }
    }

    fn is_unknown(&self) -> bool {
        matches!(&*self.inner.state.read().unwrap(), OwnerState::Unknown)
    }

    /// Applies one answer. `Unavailable` keeps the last definite owner: while the daemon
    /// is down, `whois` fails too, so that owner admits nobody. A tagged or signed-out
    /// node clears it. Unknown stays unknown, which means a token is required.
    fn commit(&self, observed: NodeOwner) -> Refresh {
        match observed {
            NodeOwner::User(login) => {
                let announce = {
                    let mut state = self.inner.state.write().unwrap();
                    let same = matches!(&*state, OwnerState::User(have) if have == &login);
                    *state = OwnerState::User(login.clone());
                    drop(state);
                    *self.inner.misses.lock().unwrap() = 0;
                    if same {
                        None
                    } else {
                        let mut announced = self.inner.announced.lock().unwrap();
                        if announced.as_deref() == Some(login.as_str()) {
                            None
                        } else {
                            *announced = Some(login.clone());
                            Some(login)
                        }
                    }
                };
                if let Some(login) = &announce {
                    tracing::info!("tailnet devices owned by {login} now connect without a token");
                }
                Refresh {
                    wait: RECHECK,
                    announce,
                    unavailable: false,
                }
            }
            NodeOwner::Nobody => {
                let changed = {
                    let mut state = self.inner.state.write().unwrap();
                    let changed = !matches!(&*state, OwnerState::Nobody);
                    *state = OwnerState::Nobody;
                    changed
                };
                *self.inner.misses.lock().unwrap() = 0;
                if changed {
                    tracing::info!(
                        "this computer is tagged or signed out of Tailscale; token required \
                         for all clients"
                    );
                }
                Refresh {
                    wait: RECHECK,
                    announce: None,
                    unavailable: false,
                }
            }
            NodeOwner::Unavailable(reason) => {
                let log = self.is_unknown()
                    && !self.inner.logged_unavailable.swap(true, Ordering::Relaxed);
                if log {
                    tracing::info!(
                        "tailscale unavailable ({reason}); token required for all clients"
                    );
                }
                let mut misses = self.inner.misses.lock().unwrap();
                let wait = backoff(*misses);
                *misses = misses.saturating_add(1);
                Refresh {
                    wait,
                    announce: None,
                    unavailable: log,
                }
            }
        }
    }

    /// One injected `self_login`, for the refresh loop and its tests.
    async fn refresh_with<F, Fut>(&self, query: F) -> Refresh
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = NodeOwner>,
    {
        self.commit(query().await)
    }

    /// Asks again until Tailscale answers, then keeps checking so a re-login or a tag lands.
    pub fn watch(&self) {
        let owner = self.clone();
        tokio::spawn(async move {
            loop {
                let wait = if let Some(_claim) = owner.claim() {
                    owner
                        .refresh_with(|| read_node_owner_within(POLL_TIMEOUT))
                        .await
                        .wait
                } else {
                    // A connection is already asking. Don't start a second status call.
                    Duration::from_millis(200)
                };
                tokio::time::sleep(wait).await;
            }
        });
    }

    /// The current owner. While it's still unknown, asks once (short timeout, and at most
    /// one lookup per [`LAZY_GAP`], shared with the background loop).
    pub async fn resolve(&self) -> Option<String> {
        self.resolve_with(Instant::now(), LAZY_GAP, || {
            read_node_owner_within(LAZY_TIMEOUT)
        })
        .await
    }

    async fn resolve_with<F, Fut>(&self, now: Instant, gap: Duration, query: F) -> Option<String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = NodeOwner>,
    {
        // A known user, or a node we already know is tagged or signed out.
        if !self.is_unknown() {
            return self.get();
        }
        // Someone is already asking. Don't start another, and don't guess an owner.
        let Some(_claim) = self.claim() else {
            return self.get();
        };
        {
            let mut last = self.inner.last_lazy.lock().unwrap();
            if last.is_some_and(|t| now.saturating_duration_since(t) < gap) {
                return None;
            }
            *last = Some(now);
        }
        let observed = query().await;
        self.commit(observed);
        self.get()
    }
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

fn is_tailscale_ip(ip: IpAddr) -> bool {
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

    async fn step(owner: &TailnetOwner, found: NodeOwner) -> Refresh {
        owner.refresh_with(move || async move { found }).await
    }

    #[tokio::test]
    async fn owner_follows_tailscale_coming_up_changing_and_tagged() {
        let owner = TailnetOwner::unknown();
        let down = || NodeOwner::Unavailable("down".into());

        let first = step(&owner, down()).await;
        assert!(first.unavailable);
        assert_eq!(first.announce, None);
        assert_eq!(first.wait, Duration::from_secs(1));
        assert_eq!(owner.get(), None, "unknown means a token is required");

        let retry = step(&owner, down()).await;
        assert!(!retry.unavailable, "retries stay quiet");
        assert_eq!(retry.wait, Duration::from_secs(2));
        assert_eq!(owner.get(), None);

        let up = step(&owner, NodeOwner::User("ada@example.com".into())).await;
        assert_eq!(up.announce.as_deref(), Some("ada@example.com"));
        assert_eq!(up.wait, Duration::from_secs(30));
        assert_eq!(owner.get().as_deref(), Some("ada@example.com"));

        let again = step(&owner, NodeOwner::User("ada@example.com".into())).await;
        assert_eq!(again.announce, None, "the same owner is logged once");
        assert_eq!(owner.get().as_deref(), Some("ada@example.com"));

        // A daemon blip is not a new owner, and not a reason to forget the last one.
        let blip = step(&owner, down()).await;
        assert!(!blip.unavailable);
        assert_eq!(blip.wait, Duration::from_secs(1));
        assert_eq!(owner.get().as_deref(), Some("ada@example.com"));

        let switched = step(&owner, NodeOwner::User("bea@example.com".into())).await;
        assert_eq!(switched.announce.as_deref(), Some("bea@example.com"));
        assert_eq!(owner.get().as_deref(), Some("bea@example.com"));

        let tagged = step(&owner, NodeOwner::Nobody).await;
        assert_eq!(tagged.announce, None);
        assert_eq!(tagged.wait, Duration::from_secs(30));
        assert_eq!(owner.get(), None, "a tagged node has no owner");

        // Untagged again: that login is the owner, and devices can skip the token once more.
        let restored = step(&owner, NodeOwner::User("ada@example.com".into())).await;
        assert_eq!(restored.announce.as_deref(), Some("ada@example.com"));
        assert_eq!(owner.get().as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn unknown_owner_backs_off_up_to_thirty_seconds() {
        let owner = TailnetOwner::unknown();
        let waits: Vec<_> = (0..8)
            .map(|_| owner.commit(NodeOwner::Unavailable("down".into())).wait)
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30].map(Duration::from_secs));
        assert_eq!(owner.get(), None);
    }

    #[tokio::test]
    async fn lazy_resolve_asks_once_while_unknown_and_not_when_settled() {
        let owner = TailnetOwner::unknown();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let now = Instant::now();
        let gap = Duration::from_secs(1);
        let ask = |found: NodeOwner| {
            let calls = &calls;
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                found
            }
        };

        let first = owner
            .resolve_with(now, gap, || ask(NodeOwner::Unavailable("down".into())))
            .await;
        assert_eq!(first, None);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let burst = owner
            .resolve_with(now + Duration::from_millis(10), gap, || {
                ask(NodeOwner::User("ada@example.com".into()))
            })
            .await;
        assert_eq!(
            burst, None,
            "a second connection inside the gap does not ask again"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(owner.get(), None);

        let later = owner
            .resolve_with(now + Duration::from_secs(2), gap, || {
                ask(NodeOwner::User("ada@example.com".into()))
            })
            .await;
        assert_eq!(later.as_deref(), Some("ada@example.com"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let settled = owner
            .resolve_with(now + Duration::from_secs(3), gap, || ask(NodeOwner::Nobody))
            .await;
        assert_eq!(settled.as_deref(), Some("ada@example.com"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "a known owner is not asked on connect"
        );

        let tagged = TailnetOwner::unknown();
        tagged.commit(NodeOwner::Nobody);
        let tagged_calls = std::sync::atomic::AtomicUsize::new(0);
        let got = tagged
            .resolve_with(now, gap, || async {
                tagged_calls.fetch_add(1, Ordering::SeqCst);
                NodeOwner::User("ada@example.com".into())
            })
            .await;
        assert_eq!(got, None);
        assert_eq!(tagged_calls.load(Ordering::SeqCst), 0);
        assert_eq!(tagged.get(), None, "tagged stays without an owner");

        let unknown = TailnetOwner::unknown();
        unknown.inner.in_flight.store(true, Ordering::SeqCst);
        let busy = std::sync::atomic::AtomicUsize::new(0);
        let blocked = unknown
            .resolve_with(now, gap, || async {
                busy.fetch_add(1, Ordering::SeqCst);
                NodeOwner::User("ada@example.com".into())
            })
            .await;
        assert_eq!(blocked, None);
        assert_eq!(
            busy.load(Ordering::SeqCst),
            0,
            "an in-flight lookup is not duplicated"
        );
        assert_eq!(unknown.get(), None);
    }

    #[test]
    fn status_json_owner_user_tagged_or_not_ready() {
        let owner = |v: &str| owner_from_status(&serde_json::from_str(v).unwrap());
        assert_eq!(
            owner(
                r#"{"BackendState":"Running","Self":{"UserID":7},"User":{"7":{"LoginName":"ada@example.com"}}}"#
            ),
            NodeOwner::User("ada@example.com".into())
        );
        assert_eq!(
            owner(
                r#"{"BackendState":"Running","Self":{"UserID":7,"Tags":["tag:server"]},"User":{"7":{"LoginName":"ada@example.com"}}}"#
            ),
            NodeOwner::Nobody,
            "a tag wins over the login on the node"
        );
        assert_eq!(
            owner(
                r#"{"BackendState":"Running","Self":{"UserID":7},"User":{"7":{"LoginName":"tagged-devices"}}}"#
            ),
            NodeOwner::Nobody
        );
        assert_eq!(
            owner(
                r#"{"BackendState":"NeedsLogin","Self":{"UserID":7},"User":{"7":{"LoginName":"ada@example.com"}}}"#
            ),
            NodeOwner::Nobody,
            "signed out, even if Self still names the previous account"
        );
        assert_eq!(
            owner(
                r#"{"BackendState":"Starting","Self":{"UserID":7},"User":{"7":{"LoginName":"ada@example.com"}}}"#
            ),
            NodeOwner::Unavailable("Starting".into())
        );
        assert!(matches!(
            owner(r#"{"BackendState":"Running","Self":{}}"#),
            NodeOwner::Unavailable(_)
        ));
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
