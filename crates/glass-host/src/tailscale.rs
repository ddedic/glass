//! Trusts peers that Tailscale says belong to the same user as this Mac, so devices on
//! your own tailnet connect without a token.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
#[cfg(test)]
use tokio::sync::Notify;

/// How often a healthy tailnet is re-read in the background, and the cap on failure backoff.
const RECHECK: Duration = Duration::from_secs(30);
/// `tailscale status` on the sign-in path. The child is killed when this fires.
const CLI_TIMEOUT: Duration = Duration::from_secs(2);
/// Concurrent status reads. Past this, a grant waits and retries; it is not a bad token.
const SLOTS: usize = 4;

/// `tailscale` on PATH, or the CLI inside the Mac App Store / standalone app bundle.
pub(crate) fn cli() -> &'static str {
    const BUNDLED: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    static CLI: std::sync::OnceLock<&str> = std::sync::OnceLock::new();
    CLI.get_or_init(|| {
        let mut version = std::process::Command::new("tailscale");
        hide_std(&mut version);
        let on_path = version.arg("version").output().is_ok_and(|o| o.status.success());
        if on_path || !std::path::Path::new(BUNDLED).exists() {
            "tailscale"
        } else {
            BUNDLED
        }
    })
}

fn hide_std(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    crate::platform::no_window(cmd);
    #[cfg(not(windows))]
    let _ = cmd;
}

fn hide_tokio(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    crate::platform::no_window_tokio(cmd);
    #[cfg(not(windows))]
    let _ = cmd;
}

fn spawn(program: &str, args: &[&str]) -> Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new(program);
    hide_tokio(&mut cmd);
    Ok(cmd
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?)
}

/// Runs `child` to completion. On timeout the process is killed and waited on, so it
/// cannot outlive the call. `kill_on_drop` covers the future being cancelled first.
async fn finish(mut child: tokio::process::Child, limit: Duration) -> Result<Vec<u8>> {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let run = async {
        tokio::try_join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait(),
        )
    };
    match tokio::time::timeout(limit, run).await {
        Ok(Ok((_, _, status))) if status.success() => Ok(out),
        Ok(Ok((_, _, _))) => anyhow::bail!("{}", String::from_utf8_lossy(&err).trim()),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            anyhow::bail!("timed out")
        }
    }
}

async fn json(args: &[&str]) -> Result<Value> {
    let mut cmd = tokio::process::Command::new(cli());
    hide_tokio(&mut cmd);
    let out = cmd.args(args).kill_on_drop(true).output().await?;
    anyhow::ensure!(
        out.status.success(),
        "tailscale {}: {}",
        args[0],
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(serde_json::from_slice(&out.stdout)?)
}

async fn json_within(args: &[&str], limit: Duration) -> Result<Value> {
    Ok(serde_json::from_slice(&finish(spawn(cli(), args)?, limit).await?)?)
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
    let mut cmd = tokio::process::Command::new(cli());
    hide_tokio(&mut cmd);
    let out = cmd.args(["serve", "--bg", &target]).kill_on_drop(true).output().await?;
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
        let mut version = std::process::Command::new(cli());
        hide_std(&mut version);
        let installed = version.arg("version").output().is_ok();
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
    let mut cmd = tokio::process::Command::new(cli());
    hide_tokio(&mut cmd);
    let out = cmd
        .arg("cert")
        .arg("--cert-file")
        .arg(&cert)
        .arg("--key-file")
        .arg(&key)
        .arg(&name)
        .kill_on_drop(true)
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

/// One `tailscale status --json` read: who owns this computer, and who `peer` is.
/// A finished read is never reused for the next grant. `Unavailable` means Tailscale
/// didn't answer (retry). `Ready` with `is_owner: false` is a definite "not the owner".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Unavailable,
    Ready {
        /// Self and this peer share one untagged UserID in this same document.
        is_owner: bool,
        owner: Option<String>,
        login: Option<String>,
        name: Option<String>,
        tags: Vec<String>,
        os: Option<String>,
    },
}

enum Host {
    User { uid: u64, login: String },
    /// Tagged, signed out, or the synthetic tagged-devices user.
    Nobody,
    Down(String),
}

/// `Running` is required before trusting a login: while Tailscale is starting, `Self` can
/// still describe the previous account. A tag wins over that login.
fn host_of(status: &Value) -> Host {
    if tagged(&status["Self"]) {
        return Host::Nobody;
    }
    match status["BackendState"].as_str() {
        Some("NeedsLogin") => Host::Nobody,
        Some("Running") => {
            let Some(uid) = status["Self"]["UserID"].as_u64() else {
                return Host::Down("no Self.UserID".into());
            };
            match login_at(status, uid) {
                Some(login) => Host::User { uid, login },
                None if login_name(status, uid).is_some() => Host::Nobody,
                None => Host::Down("no login name for self".into()),
            }
        }
        other => Host::Down(other.unwrap_or("no BackendState").into()),
    }
}

fn login_name(status: &Value, uid: u64) -> Option<&str> {
    status["User"][uid.to_string()]["LoginName"].as_str()
}

fn login_at(status: &Value, uid: u64) -> Option<String> {
    let login = login_name(status, uid)?;
    (login != "tagged-devices" && !login.is_empty()).then(|| login.to_owned())
}

fn tagged(node: &Value) -> bool {
    node["Tags"].as_array().is_some_and(|tags| !tags.is_empty())
}

fn tags_of(node: &Value) -> Vec<String> {
    node["Tags"]
        .as_array()
        .map(|tags| tags.iter().filter_map(|t| t.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn name_of(node: &Value) -> Option<String> {
    node["HostName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let short = node["DNSName"].as_str()?.trim_end_matches('.').split('.').next()?;
            (!short.is_empty()).then(|| short.to_owned())
        })
}

fn os_of(node: &Value) -> Option<String> {
    let os = node["OS"].as_str()?;
    (!os.is_empty()).then(|| os.to_ascii_lowercase())
}

fn same_ip(node: &Value, ip: IpAddr) -> bool {
    node["TailscaleIPs"].as_array().is_some_and(|ips| {
        ips.iter().any(|v| {
            v.as_str()
                .and_then(|s| s.parse::<IpAddr>().ok())
                .is_some_and(|addr| addr.to_canonical() == ip)
        })
    })
}

fn find_peer(status: &Value, ip: IpAddr) -> Option<&Value> {
    if same_ip(&status["Self"], ip) {
        return Some(&status["Self"]);
    }
    status["Peer"].as_object()?.values().find(|node| same_ip(node, ip))
}

fn peer_view(status: &Value, peer: IpAddr, host: &Host) -> View {
    let node = find_peer(status, peer);
    let peer_tagged = node.is_some_and(tagged);
    let uid = node.and_then(|n| n["UserID"].as_u64());
    let login = (!peer_tagged).then_some(uid).flatten().and_then(|id| login_at(status, id));
    let is_owner = matches!(host, Host::User { uid: owner, .. } if login.is_some() && uid == Some(*owner));
    let owner = match host {
        Host::User { login, .. } => Some(login.clone()),
        _ => None,
    };
    View::Ready {
        is_owner,
        owner,
        login,
        name: node.and_then(name_of),
        tags: node.map(tags_of).unwrap_or_default(),
        os: node.and_then(os_of),
    }
}

fn off_tailnet() -> View {
    View::Ready {
        is_owner: false,
        owner: None,
        login: None,
        name: None,
        tags: Vec::new(),
        os: None,
    }
}

fn backoff(misses: u32) -> Duration {
    Duration::from_secs((1u64 << misses.min(5)).min(RECHECK.as_secs()))
}

struct State {
    misses: u32,
    retry_at: Instant,
    announced: Option<String>,
    down: bool,
    signed_out: bool,
}

enum Source {
    Live,
    #[cfg(test)]
    Script(Arc<Script>),
}

#[cfg(test)]
struct Script {
    steps: Mutex<std::collections::VecDeque<Step>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
enum Step {
    Ok(Value),
    Err(String),
    Hold(Arc<Hold>),
}

#[cfg(test)]
struct Hold {
    entered: Notify,
    release: Notify,
    value: Value,
}

struct Inner {
    /// `Some` for `--test`: never calls Tailscale. The inner login may itself be absent.
    fixed: Option<Option<String>>,
    state: Mutex<State>,
    slots: Arc<Semaphore>,
    source: Source,
}

/// This computer's tailnet owner. Each grant reads one fresh status snapshot.
#[derive(Clone)]
pub struct TailnetOwner {
    inner: Arc<Inner>,
}

impl TailnetOwner {
    fn make(fixed: Option<Option<String>>, slots: usize, source: Source) -> Self {
        Self {
            inner: Arc::new(Inner {
                fixed,
                state: Mutex::new(State {
                    misses: 0,
                    retry_at: Instant::now(),
                    announced: None,
                    down: false,
                    signed_out: false,
                }),
                slots: Arc::new(Semaphore::new(slots)),
                source,
            }),
        }
    }

    /// No answer yet. `watch` logs when Tailscale answers; grants read status themselves.
    pub fn unknown() -> Self {
        Self::make(None, SLOTS, Source::Live)
    }

    /// A fixed owner for `--test` (`GLASS_TEST_TAILNET_OWNER`). Never refreshed.
    pub fn fixed(login: Option<String>) -> Self {
        Self::make(Some(login), SLOTS, Source::Live)
    }

    /// Asks again until Tailscale answers, then keeps checking so a re-login or a tag lands.
    pub fn watch(&self) {
        let owner = self.clone();
        tokio::spawn(async move {
            loop {
                let wait = owner.tick().await;
                tokio::time::sleep(wait).await;
            }
        });
    }

    fn paused(&self) -> Option<Duration> {
        let at = self.inner.state.lock().unwrap().retry_at;
        let now = Instant::now();
        (at > now).then(|| (at - now).max(Duration::from_millis(200)))
    }

    async fn tick(&self) -> Duration {
        if self.inner.fixed.is_some() {
            return RECHECK;
        }
        if let Some(wait) = self.paused() {
            return wait;
        }
        match self.load().await {
            None => Duration::from_secs(1),
            Some(Err(reason)) => {
                self.observe(&Host::Down(reason));
                self.paused().unwrap_or(Duration::from_secs(1))
            }
            Some(Ok(status)) => {
                self.observe(&host_of(&status));
                RECHECK
            }
        }
    }

    /// Owner and peer from one status document. Never a previously finished snapshot.
    pub async fn view(&self, peer: IpAddr) -> View {
        if let Some(owner) = &self.inner.fixed {
            return fixed_view(owner.clone());
        }
        let peer = peer.to_canonical();
        if !is_tailscale_ip(peer) || self.paused().is_some() {
            return if is_tailscale_ip(peer) { View::Unavailable } else { off_tailnet() };
        }
        let Some(read) = self.load().await else {
            return View::Unavailable;
        };
        match read {
            Err(reason) => {
                self.observe(&Host::Down(reason));
                View::Unavailable
            }
            Ok(status) => {
                let host = host_of(&status);
                self.observe(&host);
                match host {
                    Host::Down(_) => View::Unavailable,
                    _ => peer_view(&status, peer, &host),
                }
            }
        }
    }

    /// One status child, or `None` when every slot is taken. The permit is released if this
    /// future is dropped, including while the child is still running.
    async fn load(&self) -> Option<std::result::Result<Value, String>> {
        let Ok(permit) = self.inner.slots.clone().try_acquire_owned() else {
            return None;
        };
        let read = self.read().await;
        drop(permit);
        Some(read)
    }

    async fn read(&self) -> std::result::Result<Value, String> {
        match &self.inner.source {
            Source::Live => json_within(&["status", "--json"], CLI_TIMEOUT)
                .await
                .map_err(|e| format!("{e:#}")),
            #[cfg(test)]
            Source::Script(script) => script.next().await,
        }
    }

    fn observe(&self, host: &Host) {
        let mut state = self.inner.state.lock().unwrap();
        match host {
            Host::User { login, .. } => {
                if state.announced.as_deref() != Some(login.as_str()) {
                    tracing::info!("tailnet devices owned by {login} now connect without a token");
                    state.announced = Some(login.clone());
                }
                state.misses = 0;
                state.down = false;
                state.signed_out = false;
                state.retry_at = Instant::now();
            }
            Host::Nobody => {
                if !state.signed_out {
                    state.signed_out = true;
                    tracing::info!(
                        "this computer is tagged or signed out of Tailscale; token required for all clients"
                    );
                }
                state.announced = None;
                state.misses = 0;
                state.down = false;
                state.retry_at = Instant::now();
            }
            Host::Down(reason) => {
                if !state.down {
                    state.down = true;
                    tracing::info!("tailscale unavailable ({reason}); owner check will retry");
                }
                let wait = backoff(state.misses);
                state.misses = state.misses.saturating_add(1);
                state.retry_at = Instant::now() + wait;
            }
        }
    }

    #[cfg(test)]
    fn wake(&self) {
        self.inner.state.lock().unwrap().retry_at = Instant::now();
    }
}

fn fixed_view(owner: Option<String>) -> View {
    // Tests: every viewer is this tailnet user (GLASS_TEST_TAILNET_LOGIN).
    let login = std::env::var("GLASS_TEST_TAILNET_LOGIN").ok().filter(|s| !s.is_empty());
    let is_owner = login.is_some() && login == owner;
    View::Ready { is_owner, owner, login, name: None, tags: Vec::new(), os: None }
}

#[cfg(test)]
impl Script {
    async fn next(&self) -> std::result::Result<Value, String> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let step = self.steps.lock().unwrap().pop_front().ok_or("no scripted status")?;
        match step {
            Step::Ok(value) => Ok(value),
            Step::Err(reason) => Err(reason),
            Step::Hold(hold) => {
                // Arm the release waiter before announcing entry, so the test cannot
                // notify into the gap and leave this read parked forever.
                let waiting = hold.release.notified();
                tokio::pin!(waiting);
                let _ = waiting.as_mut().enable();
                hold.entered.notify_one();
                waiting.await;
                Ok(hold.value.clone())
            }
        }
    }
}

/// A tailnet device's name (e.g. "nathans-ipad"), for showing who's connected.
pub async fn device_name(peer: IpAddr) -> Option<String> {
    let status = json_within(&["status", "--json"], CLI_TIMEOUT).await.ok()?;
    find_peer(&status, peer.to_canonical()).and_then(name_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    fn doc(state: &str, self_uid: u64, self_login: &str, self_tags: &[&str], peer: &str, peer_uid: u64, peer_login: &str, peer_tags: &[&str]) -> Value {
        let mut users = serde_json::Map::new();
        users.insert(self_uid.to_string(), serde_json::json!({"LoginName": self_login}));
        users.insert(peer_uid.to_string(), serde_json::json!({"LoginName": peer_login}));
        serde_json::json!({
            "BackendState": state,
            "Self": {"UserID": self_uid, "TailscaleIPs": ["100.64.0.1"], "Tags": self_tags, "HostName": "host", "OS": "linux"},
            "Peer": {"p": {"UserID": peer_uid, "TailscaleIPs": [peer], "Tags": peer_tags, "HostName": "phone", "OS": "iOS"}},
            "User": users,
        })
    }

    fn mock(limit: usize, steps: Vec<Step>) -> (TailnetOwner, Arc<Script>) {
        let script = Arc::new(Script {
            steps: Mutex::new(steps.into()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let owner = TailnetOwner::make(None, limit, Source::Script(Arc::clone(&script)));
        (owner, script)
    }

    fn calls(script: &Script) -> usize {
        script.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[test]
    fn status_names_the_owner_only_while_running_and_untagged() {
        let ada = doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        assert!(matches!(host_of(&ada), Host::User { uid: 1, .. }));
        let tagged = doc("Running", 1, "ada@example.com", &["tag:server"], "100.64.0.8", 1, "ada@example.com", &[]);
        assert!(matches!(host_of(&tagged), Host::Nobody));
        let devices = doc("Running", 1, "tagged-devices", &[], "100.64.0.8", 1, "tagged-devices", &[]);
        assert!(matches!(host_of(&devices), Host::Nobody));
        let signed_out = doc("NeedsLogin", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        assert!(matches!(host_of(&signed_out), Host::Nobody), "signed out, even if Self still names the previous account");
        let starting = doc("Starting", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        assert!(matches!(host_of(&starting), Host::Down(_)));
        assert!(matches!(host_of(&serde_json::json!({"BackendState": "Running", "Self": {}})), Host::Down(_)));
    }

    #[test]
    fn one_snapshot_grants_only_when_peer_user_id_matches_self() {
        let peer: IpAddr = "100.64.0.8".parse().unwrap();
        let ada = doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        let view = peer_view(&ada, peer, &host_of(&ada));
        assert!(matches!(&view, View::Ready { is_owner: true, owner: Some(o), login: Some(l), name: Some(n), os: Some(os), .. } if o == "ada@example.com" && l == "ada@example.com" && n == "phone" && os == "ios"));

        // Same login string, different UserID: not the owner. The id is what status keys on.
        let same_name = doc("Running", 2, "ada@example.com", &[], "100.64.0.8", 9, "ada@example.com", &[]);
        let view = peer_view(&same_name, peer, &host_of(&same_name));
        assert!(matches!(&view, View::Ready { is_owner: false, login: Some(l), .. } if l == "ada@example.com"));

        let switched = doc("Running", 2, "bea@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        let view = peer_view(&switched, peer, &host_of(&switched));
        assert!(matches!(&view, View::Ready { is_owner: false, owner: Some(o), login: Some(l), .. } if o == "bea@example.com" && l == "ada@example.com"));

        let tagged_peer = doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &["tag:server"]);
        assert!(matches!(peer_view(&tagged_peer, peer, &host_of(&tagged_peer)), View::Ready { is_owner: false, login: None, .. }));
    }

    #[tokio::test]
    async fn a_fresh_grant_does_not_keep_the_previous_owner() {
        let peer: IpAddr = "100.64.0.8".parse().unwrap();
        let ada = doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        let bea = doc("Running", 2, "bea@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        let (owner, script) = mock(4, vec![Step::Ok(ada), Step::Ok(bea.clone())]);
        let first = owner.view(peer).await;
        assert!(matches!(first, View::Ready { is_owner: true, .. }));
        // Immediately, not after a freshness window: the next grant has to read again.
        let second = owner.view(peer).await;
        assert!(matches!(second, View::Ready { is_owner: false, owner: Some(ref o), .. } if o == "bea@example.com"), "{second:?}");
        assert_eq!(calls(&script), 2, "a finished snapshot is not reused");

        let (owner, script) = mock(1, vec![
            Step::Ok(bea),
            Step::Hold(Arc::new(Hold {
                entered: Notify::new(),
                release: Notify::new(),
                value: doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]),
            })),
        ]);
        assert!(matches!(owner.view(peer).await, View::Ready { is_owner: false, .. }));
        let hold = match script.steps.lock().unwrap().front() {
            Some(Step::Hold(hold)) => Arc::clone(hold),
            _ => panic!("expected the in-flight read"),
        };
        let lead = tokio::spawn({
            let owner = owner.clone();
            async move { owner.view(peer).await }
        });
        hold.entered.notified().await;
        let followed = tokio::time::timeout(Duration::from_secs(1), owner.view(peer)).await.expect("a busy slot must not wait out the in-flight read");
        assert!(matches!(followed, View::Unavailable), "no cached owner while a read is running: {followed:?}");
        assert_eq!(calls(&script), 2, "the follower does not start its own read");
        hold.release.notify_waiters();
        assert!(matches!(lead.await.unwrap(), View::Ready { is_owner: true, .. }));
    }

    #[tokio::test]
    async fn a_status_failure_retries_and_a_signed_out_node_needs_a_token() {
        let peer: IpAddr = "100.64.0.8".parse().unwrap();
        let signed_out = doc("NeedsLogin", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        let (owner, script) = mock(4, vec![
            Step::Err("down".into()),
            Step::Ok(signed_out),
            Step::Ok(doc("Running", 2, "bea@example.com", &[], "100.64.0.8", 2, "bea@example.com", &[])),
        ]);
        assert!(matches!(owner.view(peer).await, View::Unavailable));
        assert!(matches!(owner.view(peer).await, View::Unavailable), "backoff must not turn into a stored owner");
        assert_eq!(calls(&script), 1, "the throttled retry does not call Tailscale");
        owner.wake();
        let ready = owner.view(peer).await;
        assert!(matches!(ready, View::Ready { is_owner: false, owner: None, .. }), "signed out is definite, not unavailable: {ready:?}");
        let bea = owner.view(peer).await;
        assert!(matches!(bea, View::Ready { is_owner: true, .. }), "{bea:?}");
        assert_eq!(calls(&script), 3);
    }

    #[tokio::test]
    async fn dropping_a_status_read_frees_its_slot() {
        let peer: IpAddr = "100.64.0.8".parse().unwrap();
        let ada = doc("Running", 1, "ada@example.com", &[], "100.64.0.8", 1, "ada@example.com", &[]);
        // Not polled yet: dropping it must not take the only slot or a scripted read.
        let (owner, script) = mock(1, vec![Step::Ok(ada.clone())]);
        drop(owner.view(peer));
        assert_eq!(calls(&script), 0);
        assert!(matches!(owner.view(peer).await, View::Ready { is_owner: true, .. }));

        let hold = Arc::new(Hold { entered: Notify::new(), release: Notify::new(), value: ada });
        let (owner, script) = mock(1, vec![Step::Hold(Arc::clone(&hold)), Step::Ok(doc("Running", 2, "bea@example.com", &[], "100.64.0.8", 2, "bea@example.com", &[]))]);
        let lead = tokio::spawn({
            let owner = owner.clone();
            async move { owner.view(peer).await }
        });
        hold.entered.notified().await;
        lead.abort();
        let _ = lead.await;
        let next = tokio::time::timeout(Duration::from_secs(2), owner.view(peer)).await.expect("cancelled read must not stick");
        assert!(matches!(next, View::Ready { is_owner: true, owner: Some(ref o), .. } if o == "bea@example.com"), "{next:?}");
        assert_eq!(calls(&script), 2);
    }

    #[test]
    fn backoff_doubles_until_the_recheck_cap() {
        let waits: Vec<_> = (0..8).map(backoff).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30].map(Duration::from_secs));
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

    #[cfg(unix)]
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(windows)]
    fn process_alive(pid: u32) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .split_whitespace()
                    .any(|word| word == pid.to_string())
            })
    }

    #[tokio::test]
    async fn child_output_is_collected_and_a_hang_is_killed_and_reaped() {
        let (program, args) = if cfg!(windows) {
            ("cmd", &["/C", "echo", "glass-ok"][..])
        } else {
            ("echo", &["glass-ok"][..])
        };
        let out = finish(spawn(program, args).unwrap(), Duration::from_secs(2)).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "glass-ok");

        let (program, args) = if cfg!(windows) {
            ("ping", &["-n", "60", "127.0.0.1"][..])
        } else {
            ("sleep", &["30"][..])
        };
        let child = spawn(program, args).unwrap();
        let pid = child.id().expect("child pid");
        let started = Instant::now();
        let result = finish(child, Duration::from_millis(200)).await;
        assert!(result.is_err(), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(3), "timed out child was not killed");
        assert!(!process_alive(pid), "pid {pid} is still running");
    }
}
