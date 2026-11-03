//! Trusts peers that Tailscale says belong to the same user as this Mac, so devices on
//! your own tailnet connect without a token.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::sync::{Notify, Semaphore};

/// Background recheck once Tailscale has answered, and the cap on startup backoff.
const RECHECK: Duration = Duration::from_secs(30);
/// How recent a `tailscale status` must be before it can skip the token.
const FRESH: Duration = Duration::from_secs(2);
/// Bound on `tailscale status` and `whois`. The child is killed when this fires.
const CLI_TIMEOUT: Duration = Duration::from_secs(2);
/// Concurrent peer lookups. A sign-in flood shares these instead of spawning one each.
const WHOIS_LIMIT: usize = 4;

/// Bumped when the owner answer changes, so a whois cached for the previous account is dropped.
static OWNER_GEN: AtomicU64 = AtomicU64::new(0);

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

fn spawn(program: &str, args: &[&str]) -> Result<tokio::process::Child> {
    Ok(tokio::process::Command::new(program)
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
    let out = tokio::process::Command::new(cli())
        .args(args)
        .kill_on_drop(true)
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

async fn json_within(args: &[&str], limit: Duration) -> Result<Value> {
    Ok(serde_json::from_slice(
        &finish(spawn(cli(), args)?, limit).await?,
    )?)
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
    /// No usable answer: still starting, timed out, or the CLI failed.
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

async fn read_node_owner_within(limit: Duration) -> NodeOwner {
    match json_within(&["status", "--json"], limit).await {
        Ok(status) => owner_from_status(&status),
        Err(e) => NodeOwner::Unavailable(format!("{e:#}")),
    }
}

/// 1s, 2s, 4s, … capped at [`RECHECK`].
fn backoff(misses: u32) -> Duration {
    Duration::from_secs((1u64 << misses.min(5)).min(RECHECK.as_secs()))
}

/// One owner snapshot and the refresh that produces the next one.
struct Snap {
    generation: u64,
    login: Option<String>,
    at: Instant,
    misses: u32,
    announced: Option<String>,
    logged_down: bool,
    signed_out: bool,
    refreshing: bool,
    fixed: bool,
    wait: Duration,
}

struct Inner {
    state: Mutex<Snap>,
    wake: Notify,
}

/// Clears `refreshing` when the in-flight status read finishes, including on cancel.
struct Lead(Arc<Inner>);

impl Drop for Lead {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().refreshing = false;
        self.0.wake.notify_waiters();
    }
}

enum Decide {
    Answer(Option<String>),
    /// `at` of the snapshot that was already too old to grant. A login counts only if a
    /// read lands after this instant.
    Wait(Instant),
    Lead(Lead),
}

struct Outcome {
    wait: Duration,
    // Read by tests. Production only uses them to decide whether to log.
    #[cfg_attr(not(test), allow(dead_code))]
    logged_owner: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    logged_down: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    logged_out: bool,
}

fn blank() -> Snap {
    Snap {
        generation: 0,
        login: None,
        at: Instant::now(),
        misses: 0,
        announced: None,
        logged_down: false,
        signed_out: false,
        refreshing: false,
        fixed: false,
        wait: Duration::ZERO,
    }
}

/// This computer's tailnet owner, refreshed after startup.
///
/// `resolve` returns a login only from a current `tailscale status` (just read, or a
/// couple of seconds old). Unknown, tagged, signed out, still starting, or a failed
/// read returns `None`: the client needs a token. A failed read forgets the previous
/// owner instead of keeping them.
#[derive(Clone)]
pub struct TailnetOwner {
    inner: Arc<Inner>,
}

impl TailnetOwner {
    fn from_snap(snap: Snap) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(snap),
                wake: Notify::new(),
            }),
        }
    }

    /// No answer yet. The background loop and connection attempts fill it in.
    pub fn unknown() -> Self {
        Self::from_snap(blank())
    }

    /// A fixed owner for `--test` (`GLASS_TEST_TAILNET_OWNER`). Never refreshed.
    pub fn fixed(login: Option<String>) -> Self {
        let mut snap = blank();
        snap.fixed = true;
        snap.login = login;
        Self::from_snap(snap)
    }

    /// Last accepted login. Cleared when status fails, the node is tagged, or it signs out.
    #[cfg(test)]
    fn get(&self) -> Option<String> {
        self.inner.state.lock().unwrap().login.clone()
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.inner.state.lock().unwrap().generation
    }

    fn decide(&self, now: Instant) -> Decide {
        let mut s = self.inner.state.lock().unwrap();
        if s.fixed {
            return Decide::Answer(s.login.clone());
        }
        if s.login.is_some() && now.saturating_duration_since(s.at) < FRESH {
            return Decide::Answer(s.login.clone());
        }
        if s.refreshing {
            return Decide::Wait(s.at);
        }
        // A recent failure or a definite "no owner" is not asked again until its backoff
        // elapses, and it does not grant the previous login.
        if s.login.is_none() && now.saturating_duration_since(s.at) < s.wait {
            return Decide::Answer(None);
        }
        s.refreshing = true;
        Decide::Lead(Lead(Arc::clone(&self.inner)))
    }

    fn commit(&self, observed: NodeOwner, now: Instant) -> Outcome {
        let mut logged_owner = None;
        let mut logged_down = None;
        let mut logged_out = false;
        let wait = {
            let mut s = self.inner.state.lock().unwrap();
            s.at = now;
            match observed {
                NodeOwner::User(login) => {
                    if s.login.as_deref() != Some(login.as_str()) {
                        s.generation = s.generation.saturating_add(1);
                        OWNER_GEN.store(s.generation, Ordering::Release);
                        s.login = Some(login.clone());
                        if s.announced.as_deref() != Some(login.as_str()) {
                            s.announced = Some(login.clone());
                            logged_owner = Some(login);
                        }
                    }
                    s.misses = 0;
                    s.logged_down = false;
                    s.signed_out = false;
                    s.wait = RECHECK;
                }
                NodeOwner::Nobody => {
                    if s.login.take().is_some() {
                        s.generation = s.generation.saturating_add(1);
                        OWNER_GEN.store(s.generation, Ordering::Release);
                    }
                    if !s.signed_out {
                        s.signed_out = true;
                        logged_out = true;
                    }
                    s.misses = 0;
                    s.logged_down = false;
                    s.wait = RECHECK;
                }
                NodeOwner::Unavailable(reason) => {
                    if s.login.take().is_some() {
                        s.generation = s.generation.saturating_add(1);
                        OWNER_GEN.store(s.generation, Ordering::Release);
                    }
                    s.signed_out = false;
                    if !s.logged_down {
                        s.logged_down = true;
                        logged_down = Some(reason);
                    }
                    s.wait = backoff(s.misses);
                    s.misses = s.misses.saturating_add(1);
                }
            }
            s.wait
        };
        if let Some(login) = &logged_owner {
            tracing::info!("tailnet devices owned by {login} now connect without a token");
        }
        if logged_out {
            tracing::info!(
                "this computer is tagged or signed out of Tailscale; token required for all clients"
            );
        }
        if let Some(reason) = &logged_down {
            tracing::info!("tailscale unavailable ({reason}); token required for all clients");
        }
        Outcome {
            wait,
            logged_owner: logged_owner.is_some(),
            logged_down: logged_down.is_some(),
            logged_out,
        }
    }

    fn fresh_login(&self, now: Instant) -> Option<String> {
        let s = self.inner.state.lock().unwrap();
        if s.fixed || now.saturating_duration_since(s.at) < FRESH {
            s.login.clone()
        } else {
            None
        }
    }

    /// A waiter missed the snapshot it was born with. Grant only a read that completed
    /// while it waited, and only while that read is still current.
    fn grant_after_wait(&self, now: Instant, seen: Instant) -> Option<String> {
        let s = self.inner.state.lock().unwrap();
        if s.fixed {
            return s.login.clone();
        }
        if s.at != seen && now.saturating_duration_since(s.at) < FRESH {
            s.login.clone()
        } else {
            None
        }
    }

    async fn wait_refresh(&self) {
        let notified = self.inner.wake.notified();
        tokio::pin!(notified);
        if self.inner.state.lock().unwrap().refreshing {
            let _ = tokio::time::timeout(CLI_TIMEOUT, notified).await;
        }
    }

    fn force_lead(&self) -> Option<Lead> {
        let mut s = self.inner.state.lock().unwrap();
        if s.refreshing {
            return None;
        }
        s.refreshing = true;
        Some(Lead(Arc::clone(&self.inner)))
    }

    async fn tick(&self) -> Duration {
        if self.inner.state.lock().unwrap().fixed {
            return RECHECK;
        }
        let Some(lead) = self.force_lead() else {
            self.wait_refresh().await;
            return self
                .inner
                .state
                .lock()
                .unwrap()
                .wait
                .max(Duration::from_millis(200));
        };
        let outcome = self.commit(read_node_owner_within(CLI_TIMEOUT).await, Instant::now());
        drop(lead);
        outcome.wait
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

    /// The owner to authorize right now. A stale, missing, or in-flight answer is `None`.
    pub async fn resolve(&self) -> Option<String> {
        self.resolve_with(Instant::now(), || read_node_owner_within(CLI_TIMEOUT))
            .await
    }

    async fn resolve_with<F, Fut>(&self, now: Instant, query: F) -> Option<String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = NodeOwner>,
    {
        let lead = match self.decide(now) {
            Decide::Answer(login) => return login,
            Decide::Wait(seen) => {
                self.wait_refresh().await;
                return self.grant_after_wait(now, seen);
            }
            Decide::Lead(lead) => lead,
        };
        self.commit(query().await, now);
        let login = self.fresh_login(now);
        drop(lead);
        login
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
        .kill_on_drop(true)
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
    NoHttps { name: String },
    Ready { name: String },
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
pub async fn ensure_cert(dir: &std::path::Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
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
    // No short timeout here: issuance can be slow. Dropping the future still kills the child.
    let out = tokio::process::Command::new(cli())
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

struct Flight {
    generation: u64,
    /// `None` while the lookup is running, then the CLI result (`None` on failure).
    slot: Mutex<Option<Option<Value>>>,
    wake: Notify,
}

impl Flight {
    fn finish(&self, value: Option<Value>) {
        *self.slot.lock().unwrap() = Some(value);
        self.wake.notify_waiters();
    }

    async fn wait(&self, limit: Duration) -> Option<Value> {
        let notified = self.wake.notified();
        tokio::pin!(notified);
        if let Some(value) = self.slot.lock().unwrap().clone() {
            return value;
        }
        let _ = tokio::time::timeout(limit, notified).await;
        self.slot.lock().unwrap().clone().flatten()
    }
}

struct WhoisInner {
    cache: HashMap<IpAddr, (u64, Instant, Value)>,
    busy: HashMap<IpAddr, Arc<Flight>>,
}

struct WhoisGate {
    permits: Arc<Semaphore>,
    limit: Duration,
    inner: Mutex<WhoisInner>,
    err_at: Mutex<Option<Instant>>,
}

impl WhoisGate {
    fn new(permits: usize, limit: Duration) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(permits)),
            limit,
            inner: Mutex::new(WhoisInner {
                cache: HashMap::new(),
                busy: HashMap::new(),
            }),
            err_at: Mutex::new(None),
        }
    }

    fn log_err(&self, now: Instant) -> bool {
        let mut last = self.err_at.lock().unwrap();
        let log = last.is_none_or(|t| now.saturating_duration_since(t) >= RECHECK);
        if log {
            *last = Some(now);
        }
        log
    }

    async fn lookup<F, Fut>(
        &self,
        peer: IpAddr,
        generation: u64,
        now: Instant,
        fetch: F,
    ) -> Option<Value>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<Value>>,
    {
        enum Step {
            Join(Arc<Flight>),
            Lead(Arc<Flight>),
        }
        let step = {
            let mut g = self.inner.lock().unwrap();
            if let Some((cached_gen, at, value)) = g.cache.get(&peer)
                && *cached_gen == generation
                && now.saturating_duration_since(*at) < self.limit
            {
                return Some(value.clone());
            }
            if let Some(flight) = g.busy.get(&peer)
                && flight.generation == generation
            {
                Step::Join(Arc::clone(flight))
            } else {
                let flight = Arc::new(Flight {
                    generation,
                    slot: Mutex::new(None),
                    wake: Notify::new(),
                });
                g.busy.insert(peer, Arc::clone(&flight));
                Step::Lead(flight)
            }
        };
        let flight = match step {
            Step::Join(flight) => return flight.wait(self.limit).await,
            Step::Lead(flight) => flight,
        };
        let Ok(Ok(_permit)) =
            tokio::time::timeout(self.limit, Arc::clone(&self.permits).acquire_owned()).await
        else {
            self.retire(peer, &flight, generation, now, None);
            return None;
        };
        let value = tokio::time::timeout(self.limit, fetch()).await.ok().flatten();
        self.retire(peer, &flight, generation, now, value.clone());
        value
    }

    fn retire(
        &self,
        peer: IpAddr,
        flight: &Arc<Flight>,
        generation: u64,
        now: Instant,
        value: Option<Value>,
    ) {
        flight.finish(value.clone());
        let mut g = self.inner.lock().unwrap();
        if let Some(value) = value {
            if g.cache.len() >= 64 {
                g.cache.clear();
            }
            g.cache.insert(peer, (generation, now, value));
        }
        if g.busy
            .get(&peer)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            g.busy.remove(&peer);
        }
    }
}

fn whois_gate() -> &'static WhoisGate {
    static GATE: std::sync::OnceLock<WhoisGate> = std::sync::OnceLock::new();
    GATE.get_or_init(|| WhoisGate::new(WHOIS_LIMIT, CLI_TIMEOUT))
}

async fn fetch_whois(peer: IpAddr) -> Option<Value> {
    match json_within(&["whois", "--json", &peer.to_string()], CLI_TIMEOUT).await {
        Ok(value) => Some(value),
        Err(e) => {
            if whois_gate().log_err(Instant::now()) {
                tracing::warn!("tailscale whois {peer}: {e:#}");
            }
            None
        }
    }
}

async fn peer_record(peer: IpAddr) -> Option<Value> {
    let peer = peer.to_canonical();
    let generation = OWNER_GEN.load(Ordering::Acquire);
    whois_gate()
        .lookup(peer, generation, Instant::now(), || fetch_whois(peer))
        .await
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
    login_of(&peer_record(peer).await?)
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
    peer_record(peer).await
}

/// A tailnet device's name (e.g. "nathans-ipad"), for showing who's connected.
pub async fn device_name(peer: IpAddr) -> Option<String> {
    let who = peer_record(peer).await?;
    let name = who["Node"]["ComputedName"]
        .as_str()
        .or_else(|| who["Node"]["Hostinfo"]["Hostname"].as_str())?;
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

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
    fn unknown_owner_backs_off_and_a_failed_read_drops_the_login() {
        let owner = TailnetOwner::unknown();
        let now = Instant::now();
        let waits: Vec<_> = (0..8)
            .map(|i| {
                let outcome = owner.commit(NodeOwner::Unavailable("down".into()), now);
                assert_eq!(outcome.logged_down, i == 0);
                outcome.wait
            })
            .collect();
        assert_eq!(
            waits,
            [1, 2, 4, 8, 16, 30, 30, 30].map(Duration::from_secs)
        );
        assert_eq!(owner.get(), None);

        let ada = owner.commit(NodeOwner::User("ada@example.com".into()), now);
        assert!(ada.logged_owner);
        assert_eq!(ada.wait, RECHECK);
        assert_eq!(owner.get().as_deref(), Some("ada@example.com"));
        let generation = owner.generation();
        let again = owner.commit(NodeOwner::User("ada@example.com".into()), now);
        assert!(!again.logged_owner, "the same owner is logged once");
        assert_eq!(owner.generation(), generation, "a repeat read keeps whois usable");

        let down = owner.commit(NodeOwner::Unavailable("timed out".into()), now);
        assert!(down.logged_down);
        assert_eq!(down.wait, Duration::from_secs(1));
        assert_eq!(owner.get(), None, "a failed read forgets the owner");
        assert_ne!(owner.generation(), generation);

        assert!(
            owner
                .commit(NodeOwner::User("bea@example.com".into()), now)
                .logged_owner
        );
        let tagged = owner.commit(NodeOwner::Nobody, now);
        assert!(tagged.logged_out);
        assert_eq!(tagged.wait, RECHECK);
        assert_eq!(owner.get(), None, "a tagged node has no owner");
        assert!(!owner.commit(NodeOwner::Nobody, now).logged_out);
        assert!(
            owner
                .commit(NodeOwner::User("ada@example.com".into()), now)
                .logged_owner,
            "untagged again, that login is announced once more"
        );
    }

    #[tokio::test]
    async fn account_switch_and_a_missed_status_require_a_token() {
        let owner = TailnetOwner::unknown();
        let t = Instant::now();
        let got = owner
            .resolve_with(t, || async { NodeOwner::User("ada@example.com".into()) })
            .await;
        assert_eq!(got.as_deref(), Some("ada@example.com"));

        let calls = AtomicUsize::new(0);
        let got = owner
            .resolve_with(t + Duration::from_millis(200), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                NodeOwner::User("bea@example.com".into())
            })
            .await;
        assert_eq!(got.as_deref(), Some("ada@example.com"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a fresh status is reused");

        let got = owner
            .resolve_with(t + FRESH + Duration::from_millis(50), || async {
                NodeOwner::User("bea@example.com".into())
            })
            .await;
        assert_eq!(got.as_deref(), Some("bea@example.com"));

        let missed = t + (FRESH * 2) + Duration::from_millis(100);
        let got = owner
            .resolve_with(missed, || async {
                NodeOwner::Unavailable("Starting".into())
            })
            .await;
        assert_eq!(got, None, "Starting is not an owner");
        assert_eq!(owner.get(), None);

        let calls = AtomicUsize::new(0);
        let got = owner
            .resolve_with(missed + Duration::from_millis(20), || async {
                calls.fetch_add(1, Ordering::SeqCst);
                NodeOwner::User("bea@example.com".into())
            })
            .await;
        assert_eq!(
            got, None,
            "a throttled recheck does not restore the previous owner"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(owner.get(), None);
    }

    #[tokio::test]
    async fn in_flight_refresh_does_not_grant_the_previous_owner() {
        let owner = TailnetOwner::unknown();
        let t = Instant::now();
        owner
            .resolve_with(t, || async { NodeOwner::User("ada@example.com".into()) })
            .await
            .unwrap();
        let when = t + FRESH + Duration::from_secs(1);
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let waiting = entered.notified();
        tokio::pin!(waiting);
        let lead = tokio::spawn({
            let owner = owner.clone();
            let entered = Arc::clone(&entered);
            let release = Arc::clone(&release);
            async move {
                owner
                    .resolve_with(when, || async move {
                        entered.notify_one();
                        release.notified().await;
                        NodeOwner::User("bea@example.com".into())
                    })
                    .await
            }
        });
        waiting.await;
        release.notify_waiters();
        let calls = AtomicUsize::new(0);
        let followed = owner
            .resolve_with(when, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                NodeOwner::User("ada@example.com".into())
            })
            .await;
        let led = lead.await.unwrap();
        assert_eq!(led.as_deref(), Some("bea@example.com"));
        assert_eq!(followed.as_deref(), Some("bea@example.com"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "the in-flight read is shared");
        assert_ne!(followed.as_deref(), Some("ada@example.com"));
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

    #[test]
    fn whois_errors_are_logged_once_per_quiet_period() {
        let gate = WhoisGate::new(1, CLI_TIMEOUT);
        let t = Instant::now();
        assert!(gate.log_err(t));
        assert!(!gate.log_err(t + Duration::from_secs(1)));
        assert!(gate.log_err(t + RECHECK));
    }

    #[tokio::test]
    async fn whois_coalesces_and_drops_its_cache_when_the_owner_generation_changes() {
        let gate = Arc::new(WhoisGate::new(4, Duration::from_secs(2)));
        let peer: IpAddr = "100.70.0.1".parse().unwrap();
        let now = Instant::now();
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let waiting = entered.notified();
        tokio::pin!(waiting);

        let lead_gate = Arc::clone(&gate);
        let lead_calls = Arc::clone(&calls);
        let lead_entered = Arc::clone(&entered);
        let lead_release = Arc::clone(&release);
        let lead = tokio::spawn(async move {
            lead_gate
                .lookup(peer, 3, now, || async move {
                    lead_calls.fetch_add(1, Ordering::SeqCst);
                    lead_entered.notify_one();
                    lead_release.notified().await;
                    Some(serde_json::json!({"ok": true}))
                })
                .await
        });
        waiting.await;
        let followed_calls = Arc::clone(&calls);
        let followed = gate.lookup(peer, 3, now, || async move {
            followed_calls.fetch_add(1, Ordering::SeqCst);
            Some(serde_json::json!({"ok": false}))
        });
        release.notify_waiters();
        let (led, followed) = tokio::join!(lead, followed);
        let led = led.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one lookup serves both waiters");
        assert_eq!(led, followed);
        assert_eq!(led.unwrap()["ok"], true);

        let hit_calls = Arc::clone(&calls);
        let hit = gate
            .lookup(peer, 3, now, || async move {
                hit_calls.fetch_add(1, Ordering::SeqCst);
                Some(serde_json::json!({"ok": false}))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "same generation reuses the read");
        assert_eq!(hit.unwrap()["ok"], true);

        let next_calls = Arc::clone(&calls);
        let next = gate
            .lookup(peer, 4, now, || async move {
                next_calls.fetch_add(1, Ordering::SeqCst);
                Some(serde_json::json!({"ok": "new"}))
            })
            .await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a new owner generation misses");
        assert_eq!(next.unwrap()["ok"], "new");
    }

    #[tokio::test]
    async fn whois_fails_closed_when_the_lookup_cap_is_full() {
        let gate = Arc::new(WhoisGate::new(1, Duration::from_millis(50)));
        let hold: IpAddr = "100.64.0.1".parse().unwrap();
        let other: IpAddr = "100.64.0.2".parse().unwrap();
        let now = Instant::now();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let waiting = entered.notified();
        tokio::pin!(waiting);
        let lead_gate = Arc::clone(&gate);
        let lead_entered = Arc::clone(&entered);
        let lead_release = Arc::clone(&release);
        let lead = tokio::spawn(async move {
            lead_gate
                .lookup(hold, 1, now, || async move {
                    lead_entered.notify_one();
                    lead_release.notified().await;
                    Some(serde_json::json!({"held": true}))
                })
                .await
        });
        waiting.await;
        let blocked = gate
            .lookup(other, 1, now, || async move {
                Some(serde_json::json!({"ran": true}))
            })
            .await;
        assert_eq!(blocked, None, "past the cap, whois does not run");
        release.notify_waiters();
        assert!(lead.await.unwrap().is_some());
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

    fn echo_cmd() -> (&'static str, &'static [&'static str]) {
        #[cfg(windows)]
        {
            ("cmd", &["/C", "echo", "glass-ok"])
        }
        #[cfg(not(windows))]
        {
            ("echo", &["glass-ok"])
        }
    }

    fn hang_cmd() -> (&'static str, &'static [&'static str]) {
        #[cfg(windows)]
        {
            ("ping", &["-n", "60", "127.0.0.1"])
        }
        #[cfg(not(windows))]
        {
            ("sleep", &["30"])
        }
    }

    #[tokio::test]
    async fn child_output_is_collected_and_a_hang_is_killed_and_reaped() {
        let (program, args) = echo_cmd();
        let out = finish(spawn(program, args).unwrap(), Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out).trim(), "glass-ok");

        let (program, args) = hang_cmd();
        let child = spawn(program, args).unwrap();
        let pid = child.id().expect("child pid");
        let started = Instant::now();
        let result = finish(child, Duration::from_millis(200)).await;
        assert!(result.is_err(), "{result:?}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timed out child was not killed"
        );
        assert!(!process_alive(pid), "pid {pid} is still running");
    }
}
