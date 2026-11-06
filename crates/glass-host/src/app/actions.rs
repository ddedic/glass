//! What the app's buttons do: the same operations as the CLI commands, returning a short
//! message to show instead of printing. All blocking; the window runs them off its thread.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::{access, acl, agent, control, ipc, remote, settings};

pub fn set_setting(key: &str, value: &str) -> Result<String> {
    let mut s = settings::load();
    s.set(key, value)?;
    settings::save(&s)?;
    #[cfg(windows)]
    if key == "bind" {
        return Ok(crate::platform::firewall::bind_change_note().unwrap_or_default());
    }
    Ok(String::new())
}

pub fn set_remote(on: bool) -> Result<String> {
    remote::set_enabled(on)?;
    Ok(if on {
        "Remote access is on. Links can now work from any network.".into()
    } else {
        "Remote access is off. Links made to work from anywhere stop within seconds.".into()
    })
}

pub fn privacy(on: bool) -> Result<String> {
    ipc::call(&ipc::Request::Privacy { on })?;
    Ok(if on { "This screen is blanked while you're viewed." } else { "Privacy screen off." }.into())
}

pub fn kick(id: Option<u64>) -> Result<String> {
    let reply = ipc::call(&ipc::Request::Disconnect { id })?;
    Ok(match reply["disconnected"].as_u64().unwrap_or(0) {
        0 => "Nobody to disconnect.".into(),
        1 => "Disconnected.".into(),
        n => format!("Disconnected {n} viewers."),
    })
}

pub fn decide(id: u64, allow: bool) -> Result<String> {
    let reply = ipc::call(&ipc::Request::Decide { id, allow })?;
    anyhow::ensure!(reply["ok"] == true, "that request has already gone");
    Ok(if allow { "Let in." } else { "Turned away." }.into())
}

pub fn chat(text: &str) -> Result<String> {
    anyhow::ensure!(!text.trim().is_empty(), "type a message first");
    let reply = ipc::call(&ipc::Request::Chat { text: text.trim().into() })?;
    Ok(format!("Sent to {} viewer(s).", reply["sent"]))
}

pub fn agent(op: &str) -> Result<String> {
    ipc::call(&ipc::Request::Agent { op: Some(op.into()) })?;
    Ok(match op {
        "pause" => "Agent paused. Its next step waits for you.",
        "resume" => "Agent resumed.",
        "stop" => "Agent stopped. It can't act until you allow it again.",
        _ => "Agents may act again.",
    }
    .into())
}

/// Lets agents from other devices ask (or refuses them all).
pub fn agents_enabled(on: bool) -> Result<String> {
    crate::grants::set_enabled(on)?;
    Ok(if on { "Agents can ask to use this computer." } else { "Agents from other devices are refused, and nothing is published. Their grants are kept." }.into())
}

/// Publishes the agent sign-in and MCP endpoints with Tailscale Funnel (or stops).
pub fn agents_public(on: bool) -> Result<String> {
    Ok(match crate::oauth::publish(on)? {
        Some(url) => format!("Agents anywhere can ask at {url}"),
        None => "Agents can only reach this computer from your tailnet.".into(),
    })
}

/// Installs an app that asked for unattended access.
pub fn install_agent(id: &str) -> Result<String> {
    let name = crate::grants::update(|s| {
        let c = s.clients.iter_mut().find(|c| c.id == id && c.auth_method != "none")?;
        c.installed = true;
        c.install_requested = false;
        Some(c.name.clone())
    })?
    .ok_or_else(|| anyhow::anyhow!("that app is gone"))?;
    Ok(format!("{name} can use this computer unattended. Revoke it any time."))
}

/// Ends an agent's access (from another device).
pub fn revoke_grant(id: &str) -> Result<String> {
    anyhow::ensure!(crate::grants::revoke(Some(id))? > 0, "it was already revoked");
    Ok("Revoked. It can't use this computer unless you approve it again.".into())
}

pub fn answer(id: u64, ok: bool, note: &str) -> Result<String> {
    let note = Some(note.trim().to_string()).filter(|n| !n.is_empty());
    ipc::call(&ipc::Request::AgentAnswer { id, ok, note })?;
    Ok(String::new())
}

/// A one-time link: (url, what it allows).
pub fn link(grant: Duration, anywhere: bool, scope: access::Scope) -> Result<(String, String)> {
    let base = crate::status::read()
        .filter(|s| s.is_fresh())
        .and_then(|s| s.url)
        .context("Glass isn't running. Turn it on first.")?;
    let (url, link) = crate::make_link(&base, grant, anywhere, scope)?;
    let summary = format!(
        "Works on one device, within {}. Gives that device {} ({}).",
        access::describe(access::LINK_TTL),
        access::describe(link.grant),
        link.scope.describe()
    );
    Ok((url, summary))
}

pub fn revoke(id: Option<&str>) -> Result<String> {
    let file = access::file_for(&crate::default_token_file());
    match id {
        Some(id) => {
            anyhow::ensure!(access::revoke(&file, id)?, "that device's access has already ended");
            Ok("Revoked. That device lost access.".into())
        }
        None => {
            let n = access::revoke_all(&file)?;
            Ok(format!("Revoked {n} device{} and every unused link.", if n == 1 { "" } else { "s" }))
        }
    }
}

pub fn wake(dns: &str) -> Result<String> {
    let macs = crate::wol::wake_target(dns)?;
    Ok(format!("Sent a wake-up ({macs})."))
}

pub fn reset_token() -> Result<String> {
    let path = crate::default_token_file();
    let _ = std::fs::remove_file(&path);
    control::load_or_create_token(&path)?;
    Ok("New token made. Devices using the old one must sign in again.".into())
}

pub fn set_token(token: &str) -> Result<String> {
    control::validate_token(token.trim())?;
    control::save_token(&crate::default_token_file(), token.trim())?;
    Ok("Token saved.".into())
}

pub fn add_rule(matcher: &str, scope: access::Scope, grant: Option<Duration>) -> Result<String> {
    acl::validate(matcher)?;
    let mut rules = acl::load();
    if rules.is_empty() {
        rules = acl::effective();
    }
    rules.push(acl::Rule {
        matcher: matcher.trim().to_ascii_lowercase(),
        scope,
        duration_secs: grant.map(|d| d.as_secs()),
    });
    acl::save(&rules)?;
    Ok(String::new())
}

pub fn remove_rule(index: usize) -> Result<String> {
    let mut rules = acl::load();
    if rules.is_empty() {
        rules = acl::effective();
    }
    anyhow::ensure!(index < rules.len(), "that rule has already gone");
    rules.remove(index);
    acl::save(&rules)?;
    Ok(String::new())
}

pub fn restart() -> Result<String> {
    crate::restart_and_wait()?;
    Ok("Glass restarted. Viewers reconnect by themselves.".into())
}

/// Installs (or reinstalls) the background service and waits for it to report.
pub fn install(at_login: bool) -> Result<String> {
    agent::install(at_login)?;
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(20) {
        if crate::status::read().is_some_and(|s| s.is_fresh()) {
            return Ok(String::new());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    anyhow::bail!("Glass didn't start.{}", agent::LOG_HINT)
}

pub fn uninstall() -> Result<String> {
    agent::uninstall()?;
    Ok(format!("Glass is off and removed from {}.", if cfg!(windows) { "startup apps" } else { "login items" }))
}

/// Installs the newest release (the installer restarts the service on it) and waits for
/// it; the version now installed.
pub fn update_now() -> Result<String> {
    crate::update::run_logged()?;
    let out = std::process::Command::new(glass_path()).arg("--version").output()?;
    let version = String::from_utf8_lossy(&out.stdout).split_whitespace().nth(1).unwrap_or_default().to_string();
    anyhow::ensure!(!version.is_empty(), "the update didn't finish; see update.log in Glass's logs");
    Ok(version)
}

/// Opens the app again in a moment, once this one has quit (so it's the new version).
#[cfg(windows)]
pub fn relaunch() {
    if let Ok(exe) = std::env::current_exe() {
        let glassw = crate::platform::ps_string(&exe.with_file_name("glassw.exe").display().to_string());
        let _ = crate::platform::powershell(&format!("Start-Sleep 1; Start-Process {glassw} app")).spawn();
    }
}

/// Opens Glass.app again in a moment, once this one has quit (so it's the new version).
#[cfg(not(windows))]
pub fn relaunch() {
    use std::os::unix::process::CommandExt;
    let app = super::bundle::path();
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 1; open \"$0\"")
        .arg(&app)
        .process_group(0)
        .spawn();
}

pub fn feedback(text: &str) -> Result<String> {
    anyhow::ensure!(!text.trim().is_empty(), "write something first");
    crate::post_feedback(text.trim(), "app")?;
    Ok("Thanks! Feedback sent.".into())
}

/// Opens a link, file or folder with what the system uses for it.
pub fn open(target: &str) {
    #[cfg(windows)]
    crate::platform::setup::open_url(target);
    #[cfg(not(windows))]
    let _ = std::process::Command::new("open").arg(target).spawn();
}

/// Opens the Tailscale app (to sign in).
pub fn open_tailscale() {
    #[cfg(windows)]
    {
        let ipn = std::path::Path::new(r"C:\Program Files\Tailscale\tailscale-ipn.exe");
        let _ = std::process::Command::new(ipn).spawn();
    }
    #[cfg(not(windows))]
    let _ = std::process::Command::new("open").args(["-a", "Tailscale"]).spawn();
}

/// Where Tailscale is downloaded for this system.
pub const TAILSCALE_DOWNLOAD: &str =
    if cfg!(windows) { "https://tailscale.com/download/windows" } else { "https://tailscale.com/download/mac" };

/// The `glass` an agent should run: the installed one, else this binary.
pub fn glass_path() -> PathBuf {
    #[cfg(windows)]
    let installed = crate::platform::local_app_data().join(r"Programs\Glass\bin\glass.exe");
    #[cfg(not(windows))]
    let installed = crate::home().join(".glass/bin/glass");
    if installed.exists() { installed } else { std::env::current_exe().unwrap_or(installed) }
}

// ---- Agents: adding `glass mcp` to MCP clients ----

#[derive(Clone, Copy, PartialEq)]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
}

impl Client {
    pub const ALL: [Client; 3] = [Client::ClaudeCode, Client::ClaudeDesktop, Client::Cursor];

    pub fn name(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Cursor => "Cursor",
        }
    }

    fn config(self) -> Option<PathBuf> {
        let home = crate::home();
        match self {
            Client::ClaudeCode => None,
            Client::ClaudeDesktop => Some(home.join("Library/Application Support/Claude/claude_desktop_config.json")),
            Client::Cursor => Some(home.join(".cursor/mcp.json")),
        }
    }

    /// Whether the client is on this computer at all.
    pub fn present(self) -> bool {
        match self {
            Client::ClaudeCode => claude_cli().is_some(),
            Client::ClaudeDesktop => {
                std::path::Path::new("/Applications/Claude.app").exists()
                    || self.config().is_some_and(|c| c.parent().is_some_and(|d| d.exists()))
            }
            Client::Cursor => {
                std::path::Path::new("/Applications/Cursor.app").exists() || crate::home().join(".cursor").exists()
            }
        }
    }

    /// Glass is already one of its MCP servers.
    pub fn added(self) -> bool {
        match self {
            Client::ClaudeCode => {
                let Some(claude) = claude_cli() else { return false };
                std::fs::read_to_string(crate::home().join(".claude.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .is_some_and(|v| v["mcpServers"]["glass"].is_object())
                    || std::process::Command::new(claude)
                        .args(["mcp", "get", "glass"])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success())
            }
            _ => self
                .config()
                .and_then(|c| std::fs::read_to_string(c).ok())
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .is_some_and(|v| v["mcpServers"]["glass"].is_object()),
        }
    }

    pub fn add(self) -> Result<String> {
        let glass = glass_path();
        match self {
            Client::ClaudeCode => {
                let claude = claude_cli().context("Claude Code isn't installed")?;
                let out = std::process::Command::new(claude)
                    .args(["mcp", "add", "-s", "user", "glass", "--"])
                    .arg(&glass)
                    .arg("mcp")
                    .output()?;
                let err = String::from_utf8_lossy(&out.stderr);
                anyhow::ensure!(out.status.success() || err.contains("already exists"), "{}", err.trim());
            }
            _ => {
                let path = self.config().expect("a config file");
                let mut config: Value = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!({}));
                anyhow::ensure!(config.is_object(), "{} isn't a JSON object; fix or remove it first", path.display());
                if !config["mcpServers"].is_object() {
                    config["mcpServers"] = json!({});
                }
                config["mcpServers"]["glass"] = json!({ "command": glass, "args": ["mcp"] });
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
            }
        }
        Ok(format!(
            "Added to {}.{}",
            self.name(),
            if self == Client::ClaudeCode { " New sessions can use this computer." } else { " Restart it to pick Glass up." }
        ))
    }
}

/// `claude`, wherever it's installed: apps don't get the shell's PATH.
fn claude_cli() -> Option<PathBuf> {
    let home = crate::home();
    [
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        "/opt/homebrew/bin/claude".into(),
        "/usr/local/bin/claude".into(),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// What to paste into any other MCP client.
pub fn mcp_json() -> String {
    serde_json::to_string_pretty(&json!({ "mcpServers": { "glass": { "command": glass_path(), "args": ["mcp"] } } }))
        .unwrap_or_default()
}
