mod access;
mod account;
mod actions;
mod fleet;
#[cfg(any(target_os = "macos", windows))]
mod app;
mod computer;
mod admin;
mod acl;
mod approval;
mod browse;
mod clips;
mod codec;
mod connlog;
mod cursor;
mod elements;
mod control;
mod damage;
mod files;
mod flow;
mod forward;
mod grants;
mod gamepad;
mod ipc;
mod keys;
mod ldap;
mod lid;
mod listen;
mod firewall_bind;
mod lossless;
#[cfg(not(target_os = "macos"))]
mod mailbox;
mod mcp;
mod oauth;
mod media;
mod mic;
mod notify;
mod now_playing;
mod opus_audio;
mod pcm;
mod oidc;
mod origin;
mod passkey;
mod policy;
mod posture;
mod power;
mod privacy;
mod clip;
mod push;
mod recording;
mod scim;
mod settings;
mod signin;
mod sharp;
mod terminal;
mod wol;
mod rate;
mod refresh;
mod remote;
mod resolution;
mod retransmit;
mod rtp;
mod server;
mod setup;
mod soft_encode;
mod status;
mod support;
mod tailscale;
mod testmode;
mod thumbs;
mod update;
mod viewers;
mod zipdir;
#[cfg(any(windows, test))]
mod win_capture;

#[cfg(target_os = "macos")]
#[path = "macos/mod.rs"]
mod platform;
#[cfg(target_os = "linux")]
#[path = "linux/mod.rs"]
mod platform;
#[cfg(windows)]
#[path = "windows/mod.rs"]
mod platform;
// The Windows port's pure parts, so their tests run on every OS.
#[cfg(all(test, not(windows)))]
#[path = "windows/convert.rs"]
#[allow(dead_code)]
mod windows_convert;

// Platform modules keep the same names everywhere, so shared code says `crate::input` etc.
use platform::{agent, audio, clipboard, clock, input, permissions, video};

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::Parser;
use tokio::sync::broadcast;
use tracing::info;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

use crate::media::VideoSource;

#[derive(Parser)]
#[command(
    name = "glass",
    about = "Low-latency remote desktop for your computer",
    version
)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    /// Port for the web client (TCP).
    #[arg(long, default_value_t = 8080)]
    http_port: u16,
    /// Also serve the web client here when it's free, so links need no port (standard
    /// HTTPS). 0 turns it off. Only with TLS; on Linux `glass setup` uses `tailscale serve`.
    #[arg(long, default_value_t = 443)]
    https_port: u16,
    /// Port for WebTransport (UDP).
    #[arg(long, default_value_t = 4433)]
    wt_port: u16,
    /// Addresses to listen on, for the web client and WebTransport. Repeat the flag or
    /// separate addresses with commas (`127.0.0.1,100.64.0.2`). `tailscale` uses this
    /// machine's Tailscale addresses and fails if Tailscale isn't running. Default:
    /// `0.0.0.0`. On macOS, port 443 needs the default bind (or `0.0.0.0`). When set,
    /// overrides `glass config bind`.
    #[arg(long, value_name = "ADDR", value_delimiter = ',')]
    bind: Vec<String>,
    /// Capture frame rate cap. Viewers get 60, or up to 120 on a fast screen (macOS).
    #[arg(long, default_value_t = 120)]
    fps: i32,
    /// Target bitrate in Mbps.
    #[arg(long, default_value_t = 30)]
    mbps: i32,
    /// Output scale relative to the display's native pixel size.
    #[arg(long, default_value_t = 1.0)]
    scale: f64,
    /// TLS certificate (PEM). Defaults to an automatic `tailscale cert` for this Mac when
    /// Tailscale HTTPS is enabled; without any, the web client is served over plain HTTP,
    /// which browsers only treat as secure on localhost.
    #[arg(long, requires = "key")]
    cert: Option<PathBuf>,
    #[arg(long, requires = "cert")]
    key: Option<PathBuf>,
    /// Host the browser should connect WebTransport to, when it differs from the page's
    /// host (e.g. page served through a tunnel, video direct over the LAN). GLASS_WT_HOST
    /// sets it too, for a host started by the background service.
    #[arg(long, env = "GLASS_WT_HOST")]
    wt_host: Option<String>,
    /// Test mode: a generated test pattern instead of screen capture, recorded input, an
    /// in-memory clipboard, no Tailscale, and /test/* hooks. Never touches the real desktop.
    #[arg(long)]
    test: bool,
    /// Where received files are saved (default ~/Downloads).
    #[arg(long)]
    downloads_dir: Option<PathBuf>,
    /// Web client files. Defaults to the installed copy next to the binary, or the source tree.
    #[arg(long)]
    web_dir: Option<PathBuf>,
    /// Access token file; created with a random token on first run.
    #[arg(long, default_value_os_t = default_token_file())]
    token_file: PathBuf,
    /// Where host.log and client.log (forwarded from browsers) are written. Defaults to
    /// ~/Library/Logs/Glass (macOS) or ~/.local/state/glass (Linux) when installed, or logs/
    /// in the source tree.
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Guided setup: Tailscale, background service, permissions, then how to connect.
    Setup {
        /// Non-interactive (for scripts and AI agents): do every step that needs no
        /// human, then report what's left. Uses sudo only if it needs no password.
        #[arg(long)]
        yes: bool,
        /// With --yes: print the report as JSON.
        #[arg(long, requires = "yes")]
        json: bool,
    },
    /// Open the Glass app: setup, who's connected, links, settings and more, in a window.
    #[cfg(any(target_os = "macos", windows))]
    App {
        /// Just (re)create Glass.app in ~/Applications (on Windows, the Start menu shortcut).
        #[arg(long)]
        install: bool,
    },
    /// Get help from someone: shows a code they type at glassdesk.dev/help to see and
    /// control this computer, after you allow them. Nothing stays once you stop it.
    Support {
        /// No window: the code and questions in this terminal.
        #[arg(long)]
        no_window: bool,
        /// One JSON line {code, url, http_port, dir}, then wait (scripts and tests).
        #[arg(long)]
        json: bool,
        /// Test mode host (a test pattern, recorded input).
        #[arg(long, hide = true)]
        test: bool,
    },
    /// Show whether the background service is running and what it can do.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Send feedback to the Glass developers.
    Feedback { message: Vec<String> },
    /// MCP server (stdio) letting an AI agent see and use this computer, or another one on
    /// your tailnet; you watch and can pause, stop or answer it from the viewer.
    /// Add it to Claude Code: claude mcp add glass -- glass mcp
    Mcp {
        /// Another computer running Glass, by tailnet name (mac-mini) or full name.
        #[arg(long)]
        machine: Option<String>,
        /// Glass's address, e.g. http://127.0.0.1:8080 (instead of --machine).
        #[arg(long)]
        url: Option<String>,
        /// The token file to sign in with (default: this computer's).
        #[arg(long)]
        token_file: Option<PathBuf>,
    },
    /// Agents from anywhere (MCP over HTTP with OAuth, e.g. Vercel Connect or claude.ai):
    /// list the ones you approved, revoke them, install one for unattended use, or read
    /// what they did.
    Agents {
        #[arg(value_parser = ["list", "revoke", "install", "log", "verify", "on", "off", "public"], default_value = "list")]
        action: String,
        /// A grant's id (revoke; `all` for every one), a client's id (install), or on/off (public).
        id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Restart the background service (viewers reconnect by themselves).
    Restart,
    /// Stop the background service and remove it from login items.
    Uninstall {
        /// Windows: remove everything Glass added except its settings (Settings › Apps runs this).
        #[arg(long, hide = true)]
        everything: bool,
    },
    /// Install the latest release (and restart the service onto it).
    Update,
    /// Make a one-time magic link that lets a device in without the token.
    Link {
        /// How long the access lasts once the link is opened (e.g. 30m, 24h, 7d).
        #[arg(long = "for", value_name = "DURATION", default_value = "24h", value_parser = access::parse_duration)]
        grant: std::time::Duration,
        /// Cut off one device (by the id from --list), or with no id every device let in
        /// with a magic link, voiding unused links too.
        #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
        revoke: Option<String>,
        /// List the devices let in with magic links.
        #[arg(long)]
        list: bool,
        /// Watch only: no mouse, keyboard or controllers.
        #[arg(long)]
        view_only: bool,
        /// No sending or getting files.
        #[arg(long)]
        no_files: bool,
        /// No copy and paste.
        #[arg(long)]
        no_clipboard: bool,
        /// Ask the person at this computer before each connection.
        #[arg(long)]
        ask: bool,
        /// The device must register a passkey (Face ID, Touch ID, a security key) and
        /// use it every time.
        #[arg(long)]
        passkey: bool,
        /// Print JSON ({url, link_expires, grant_secs, summary}) for scripts and agents.
        #[arg(long)]
        json: bool,
        /// A link that works from anywhere, not just your tailnet (over WebRTC; turns on
        /// remote access).
        #[arg(long)]
        anywhere: bool,
    },
    /// Remote access from outside your tailnet (WebRTC via glassdesk.dev): on, off or status.
    Remote {
        #[arg(value_parser = ["on", "off", "status"], default_value = "status")]
        action: String,
    },
    /// Your computers at glassdesk.dev: add this one to the account you're signed in to
    /// there (a one-time link), or remove it from every account.
    Account {
        #[arg(value_parser = ["add", "remove"])]
        action: String,
        #[arg(long)]
        json: bool,
    },
    /// Tell your glassdesk.dev account what state this computer is in (model, system,
    /// memory, disk, encryption, firewall, uptime, battery), signed by this computer. Off
    /// until you turn it on.
    Fleet {
        #[arg(value_parser = ["on", "off", "status", "report", "manage"], default_value = "status")]
        action: String,
        /// With `manage`: `off` or `status`; otherwise a link to let your account run actions here.
        arg: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Check that a virtual display (for "fit this device" resolutions) can be created and
    /// removed cleanly: mirrors the main display onto one for a few seconds, then restores.
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    DisplayTest {
        /// Size, e.g. 2048x1536.
        size: String,
        #[arg(long, default_value_t = 6)]
        seconds: u64,
    },
    /// Access rules for directory sign-in (SSO, LDAP): who gets in, and what they may do.
    Acl {
        #[command(subcommand)]
        action: Option<AclCommand>,
        #[arg(long)]
        json: bool,
    },
    /// Show or change settings (no arguments: list them all).
    Config {
        key: Option<String>,
        value: Option<String>,
    },
    /// Connection history: who connected, when, how, and for how long.
    Log {
        #[arg(short = 'n', default_value_t = 20)]
        count: usize,
        #[arg(long)]
        json: bool,
    },
    /// Who's connected right now.
    Viewers {
        #[arg(long)]
        json: bool,
    },
    /// End a session (by the id from `glass viewers`), or every one.
    Kick { id: Option<u64> },
    /// Connections waiting to be allowed (see `glass config ask-guests`).
    Requests {
        #[arg(long)]
        json: bool,
    },
    /// Let a waiting connection in (the oldest, without an id).
    Allow { id: Option<u64> },
    /// Turn a waiting connection away (the oldest, without an id).
    Deny { id: Option<u64> },
    /// Send a message to everyone viewing.
    Chat { message: Vec<String> },
    /// Viewers' microphones on this computer (macOS: Glass Microphone; Windows: VB-CABLE):
    /// status, or install what they need.
    #[cfg(any(target_os = "macos", windows))]
    Mic {
        #[arg(value_parser = ["status", "install", "uninstall"], default_value = "status")]
        action: String,
    },
    /// Devices that get notifications (turned on in each device's viewer): list, test, or
    /// turn one off.
    Notifications {
        #[arg(value_parser = ["list", "test", "off"], default_value = "list")]
        action: String,
        /// A device's id (from the list).
        id: Option<String>,
    },
    /// Blank this screen and block local input while viewed (or stop): on or off.
    Privacy {
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// The secure-desktop service (LocalSystem): lets viewers use the lock screen, answer
    /// UAC prompts, control elevated windows and send Ctrl+Alt+Del. Install or remove it
    /// (needs administrator; `glass setup` asks for you).
    #[cfg(windows)]
    Service {
        #[arg(value_parser = ["install", "uninstall", "status"], default_value = "status")]
        action: String,
        /// MSI full uninstall only: also remove the installed program's firewall rule.
        #[arg(long, hide = true)]
        remove_host_rule: bool,
        /// Classified before elevation. The elevated install does not read settings.
        #[arg(long, hide = true, value_parser = ["tailscale", "loopback", "any"])]
        firewall_scope: Option<String>,
    },
    /// Wake a machine with Wake-on-LAN, by a MAC address or a tailnet name seen before.
    Wake { target: String },
    /// Show the access token for devices outside your tailnet.
    Token {
        /// Replace it with a new one; devices using the old token must sign in again.
        #[arg(long)]
        reset: bool,
        /// Choose the token yourself (asked for without echo, or read from stdin).
        #[arg(long, conflicts_with = "reset")]
        set: bool,
    },
}

/// A one-time link to this host: on the tailnet (`base` is the host's own address), or from
/// anywhere over WebRTC, which turns remote access on.
pub(crate) fn make_link(
    base: &str,
    grant: std::time::Duration,
    anywhere: bool,
    scope: access::Scope,
) -> Result<(String, access::NewLink)> {
    let link = access::create_link(&access::file_for(&default_token_file()), grant, scope)?;
    let url = if anywhere {
        remote::set_enabled(true)?;
        remote::link_url(&remote::HostKey::load_or_create()?, &link.code)
    } else {
        format!("{}/#l={}", base.trim_end_matches('/'), link.code)
    };
    Ok((url, link))
}

/// `glass link`: a one-time link (and QR code) to the running host.
fn print_link(
    grant: std::time::Duration,
    json: bool,
    anywhere: bool,
    scope: access::Scope,
) -> Result<()> {
    let base = status::read()
        .filter(|s| s.is_fresh())
        .and_then(|s| s.url)
        .ok_or_else(|| {
            anyhow::anyhow!("Glass isn't running here. Start it (glass setup), then try again.")
        })?;
    let (url, link) = make_link(&base, grant, anywhere, scope)?;
    if json {
        let out = serde_json::json!({
            "url": url,
            "link_expires": link.expires,
            "grant_secs": link.grant.as_secs(),
            "scope": link.scope,
            "summary": format!("Works on one device, within {}. Gives that device {} of access ({}).", access::describe(access::LINK_TTL), access::describe(link.grant), link.scope.describe()),
        });
        println!("{out}");
        return Ok(());
    }
    println!("{}", setup::qr(&url));
    println!("{url}\n");
    println!(
        "Works on one device, within {}. Gives that device {} of access ({}).\nSee who's in: glass link --list · cut off everyone: glass link --revoke",
        access::describe(access::LINK_TTL),
        access::describe(link.grant),
        link.scope.describe(),
    );
    Ok(())
}

/// `glass link --list`.
fn list_passes(json: bool) -> Result<()> {
    let passes = access::list(&access::file_for(&default_token_file()));
    if json {
        println!("{}", serde_json::json!({ "passes": passes }));
        return Ok(());
    }
    if passes.is_empty() {
        println!("No devices have magic-link access.");
        return Ok(());
    }
    let now = access::now();
    for p in passes {
        let device = if p.device.is_empty() { "A browser" } else { &p.device };
        println!(
            "{}  {device}  {}  {} left",
            p.id,
            p.scope.describe(),
            access::describe(std::time::Duration::from_secs(p.expires.saturating_sub(now))),
        );
    }
    println!("\nCut one off: glass link --revoke <id>");
    Ok(())
}

#[derive(clap::Subcommand)]
enum AclCommand {
    /// Add a rule at the end (or at --at): an email, @domain, group:name or *.
    Add {
        #[arg(value_name = "MATCH")]
        matcher: String,
        #[arg(long)]
        view_only: bool,
        #[arg(long)]
        no_files: bool,
        #[arg(long)]
        no_clipboard: bool,
        /// Ask the person at the computer each time.
        #[arg(long)]
        ask: bool,
        /// How long a sign-in lasts (default 24h).
        #[arg(long = "for", value_name = "DURATION", value_parser = access::parse_duration)]
        grant: Option<std::time::Duration>,
        /// Position (0 = first); rules are checked in order.
        #[arg(long)]
        at: Option<usize>,
    },
    /// Remove the rule at this position (see glass acl).
    Remove { index: usize },
}

/// `glass acl [add|remove]`.
fn acl_cli(action: Option<&AclCommand>, json: bool) -> Result<()> {
    let mut rules = acl::load();
    match action {
        None => {
            let implicit = rules.is_empty();
            let shown = if implicit { acl::effective() } else { rules };
            if json {
                println!("{}", serde_json::json!({ "rules": shown, "implicit": implicit }));
                return Ok(());
            }
            if shown.is_empty() {
                println!("No access rules: nobody can sign in with a directory account.\nAdd one: glass acl add @example.com");
            } else {
                if implicit {
                    println!("From oidc-allow (glass acl add … replaces these):");
                }
                for (i, r) in shown.iter().enumerate() {
                    println!("{}", acl::describe(i, r));
                }
            }
            return Ok(());
        }
        Some(AclCommand::Add { matcher, view_only, no_files, no_clipboard, ask, grant, at }) => {
            acl::validate(matcher)?;
            // The first explicit rule takes over from the implicit SSO ones: keep them.
            if rules.is_empty() {
                rules = acl::effective();
            }
            let rule = acl::Rule {
                matcher: matcher.trim().to_ascii_lowercase(),
                scope: access::Scope {
                    view_only: *view_only,
                    files: !*no_files,
                    clipboard: !*no_clipboard,
                    ask: *ask,
                    passkey: false,
                    support: false,
                },
                duration_secs: grant.map(|d| d.as_secs()),
            };
            let i = at.unwrap_or(rules.len()).min(rules.len());
            rules.insert(i, rule);
        }
        Some(AclCommand::Remove { index }) => {
            if rules.is_empty() {
                rules = acl::effective();
            }
            anyhow::ensure!(*index < rules.len(), "no rule {index}; see glass acl");
            rules.remove(*index);
        }
    }
    acl::save(&rules)?;
    if json {
        println!("{}", serde_json::json!({ "rules": rules }));
    } else {
        for (i, r) in rules.iter().enumerate() {
            println!("{}", acl::describe(i, r));
        }
    }
    Ok(())
}

/// `glass config [key [value]]`.
fn config_cli(key: Option<&str>, value: Option<&str>) -> Result<()> {
    let mut s = settings::load();
    match (key, value) {
        (None, _) => {
            let policy = policy::load();
            for (k, help) in settings::KEYS {
                let managed = if policy.locked(k) { format!(" ({})", policy::MANAGED) } else { String::new() };
                println!("{k:<16} {:<12} {help}{managed}", s.get(k)?);
            }
            // Policies on things that aren't settings here (remote access, files…).
            let others: Vec<_> = policy.iter().filter(|(d, _)| !settings::KEYS.iter().any(|(k, _)| *k == d.key)).collect();
            if !others.is_empty() {
                println!("\nAlso {}:", policy::MANAGED);
                for (d, v) in others {
                    println!("{:<16} {v:<12} {}", d.key, d.what);
                }
            }
        }
        (Some(k), None) => println!("{}", s.get(k)?),
        (Some(k), Some(v)) => {
            s.set(k, v)?;
            settings::save(&s)?;
            println!("{k} = {}", s.get(k)?);
            #[cfg(windows)]
            if k == "bind"
                && let Some(note) = platform::firewall::bind_change_note()
            {
                println!("{note}");
            }
            // A generated token is shown once, to paste into the identity provider.
            if k == "scim-token" && v == "new"
                && let Some(t) = settings::scim_token()
            {
                println!("SCIM token: {t}\nIn your identity provider: base URL <this computer's https address>/scim/v2, bearer token as above.");
            }
        }
    }
    Ok(())
}

/// `glass allow|deny [id]`.
fn decide_cli(id: Option<u64>, allow: bool) -> Result<()> {
    let id = match id {
        Some(id) => id,
        None => {
            let pending = ipc::call(&ipc::Request::Pending)?;
            pending["pending"][0]["id"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("nobody is waiting to connect"))?
        }
    };
    let reply = ipc::call(&ipc::Request::Decide { id, allow })?;
    anyhow::ensure!(reply["ok"] == true, "no waiting connection {id}");
    println!("{}", if allow { "Allowed." } else { "Denied." });
    Ok(())
}

/// `glass restart`: restarts the service and waits until it's back.
fn restart_service() -> Result<()> {
    let pid = restart_and_wait()?;
    println!("Glass restarted (pid {pid}). Viewers reconnect by themselves.");
    Ok(())
}

/// Restarts the service; returns its new pid once it reports in.
pub(crate) fn restart_and_wait() -> Result<u32> {
    anyhow::ensure!(agent::is_installed(), "Glass isn't installed as a background service; run `glass setup`");
    let before = status::read().map(|s| s.pid);
    agent::restart()?;
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Some(s) = status::read()
            && Some(s.pid) != before
            && s.is_fresh()
        {
            return Ok(s.pid);
        }
    }
    anyhow::bail!("Glass hasn't come back yet; check `glass status` and the logs")
}

/// `glass remote on|off|status`.
fn remote_cli(action: &str) -> Result<()> {
    match action {
        "on" => {
            remote::set_enabled(true)?;
            println!("Remote access is on. Make a link with: glass link --anywhere");
        }
        "off" => {
            remote::set_enabled(false)?;
            println!(
                "Remote access is off. Links made with --anywhere stop working within seconds; tailnet access is unaffected."
            );
        }
        _ => println!(
            "Remote access is {}.",
            if remote::enabled() {
                "on (glass remote off to stop)"
            } else {
                "off (glass remote on, or glass link --anywhere)"
            }
        ),
    }
    Ok(())
}

fn account_cli(action: &str, json: bool) -> Result<()> {
    if action == "remove" {
        let n = account::remove()?;
        if json {
            println!("{}", serde_json::json!({ "removed": n }));
        } else if n == 0 {
            println!("This computer isn't on any account.");
        } else {
            println!("Removed this computer from {n} account{}.", if n == 1 { "" } else { "s" });
        }
        return Ok(());
    }
    let claim = account::add()?;
    if json {
        println!("{}", serde_json::json!({ "url": claim.url, "name": claim.name, "expires": claim.expires }));
        return Ok(());
    }
    println!(
        "Open this where you're signed in to glassdesk.dev/computers to add {} to your account (works once, for 15 minutes):\n\n{}\n{}",
        claim.name,
        setup::qr(&claim.url),
        claim.url
    );
    Ok(())
}

fn fleet_cli(action: &str, arg: Option<&str>, json: bool) -> Result<()> {
    match action {
        "manage" => return fleet_manage(arg, json),
        "on" => {
            fleet::set_enabled(true)?;
            println!(
                "This computer will tell your glassdesk.dev account its model, system, memory, disk space, FileVault and firewall state, uptime, battery and Glass version, every 15 minutes (only if it's on an account: glass account add). Nothing else leaves it. Turn it off, and delete what was sent, with: glass fleet off"
            );
            match fleet::send(false) {
                Ok(_) => println!("First report sent."),
                Err(e) => println!("Not sent yet: {e}"),
            }
        }
        "off" => {
            fleet::set_enabled(false)?;
            let deleted = fleet::send(true).is_ok();
            println!("{}", if deleted { "Stopped. What was sent has been deleted." } else { "Stopped. (Couldn't reach glassdesk.dev to delete what was sent; it goes when you run glass account remove.)" });
        }
        "report" => {
            let inv = fleet::send(false)?;
            if json {
                println!("{}", serde_json::to_string(&inv)?);
            } else {
                println!("Sent: {}", serde_json::to_string_pretty(&inv)?);
            }
        }
        _ => {
            let on = fleet::enabled();
            let last = fleet::last_sent();
            if json {
                println!("{}", serde_json::json!({ "enabled": on, "last_sent_secs_ago": last }));
            } else if !on {
                println!("Fleet reporting is off (glass fleet on).");
            } else {
                match last {
                    Some(s) => println!("Fleet reporting is on; last sent {} min ago.", s / 60),
                    None => println!("Fleet reporting is on; nothing sent yet (the computer must be on an account: glass account add)."),
                }
            }
        }
    }
    Ok(())
}

/// `glass fleet manage [off|status]`: let your account run actions here, signed by a passkey.
fn fleet_manage(arg: Option<&str>, json: bool) -> Result<()> {
    match arg {
        Some("off") => {
            let n = actions::forget_managers()?;
            println!("Stopped taking actions. {n} passkey{} forgotten.", if n == 1 { "" } else { "s" });
        }
        Some("status") => {
            let recent = actions::recent(5);
            if json {
                println!("{}", serde_json::json!({ "passkeys": actions::managers_count(), "recent": recent }));
            } else {
                println!("{} passkey{} can run actions here.", actions::managers_count(), if actions::managers_count() == 1 { "" } else { "s" });
                for r in recent {
                    println!("  {} {} {} {}", r["t"], if r["ok"] == true { "ok " } else { "no " }, r["action"].as_str().unwrap_or(""), r["message"].as_str().unwrap_or(""));
                }
            }
        }
        None => {
            anyhow::ensure!(remote::enabled(), "remote access is off, and actions arrive over it. Turn it on with: glass remote on");
            // The account's page offers actions on computers that say they can take them.
            fleet::set_enabled(true)?;
            let url = actions::start_enrollment()?;
            if json {
                println!("{}", serde_json::json!({ "url": url, "expires_secs": 900 }));
            } else {
                println!(
                    "This also turns on fleet reporting (glass fleet off stops it). Open this where you're signed in to glassdesk.dev/computers (works once, for 15 minutes) and confirm with your passkey. Your account can then ask this computer to refresh its details, update or restart Glass, restart the computer, or run scripts you put in ~/.config/glass/fleet/scripts, each confirmed with your passkey. Stop with: glass fleet manage off\n\n{}\n{}",
                    setup::qr(&url),
                    url
                );
            }
        }
        Some(other) => anyhow::bail!("glass fleet manage [off|status], not {other}"),
    }
    Ok(())
}

/// `glass token --set`: prompts on a terminal, otherwise reads one line from stdin.
fn set_token(path: &std::path::Path) -> Result<()> {
    use std::io::IsTerminal;
    let token = if std::io::stdin().is_terminal() {
        let first = cliclack::password("New access token")
            .mask('•')
            .validate(|t: &String| control::validate_token(t).map_err(|e| e.to_string()))
            .interact()?;
        let again = cliclack::password("Again, to confirm")
            .mask('•')
            .interact()?;
        anyhow::ensure!(first == again, "the tokens don't match; nothing changed");
        first
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        line.trim().to_string()
    };
    control::save_token(path, &token)?;
    eprintln!(
        "Token saved. The running host uses it for new connections right away; devices using the old token must sign in again."
    );
    Ok(())
}

const FEEDBACK_URL: &str = "https://glassdesk.dev/api/feedback";

fn main() -> Result<()> {
    // Windows: the helper holding the local-input hooks (see platform::front).
    #[cfg(windows)]
    if let [_, flag, pid] = std::env::args().collect::<Vec<_>>().as_slice()
        && flag == platform::front::HOOK_HELPER_ARG
    {
        return platform::front::run_hook_helper(pid);
    }
    // Windows: the secure-desktop service (started by the SCM) and the SYSTEM helper it
    // keeps running in the active console session (see platform::service / platform::secure).
    #[cfg(windows)]
    {
        let argv: Vec<String> = std::env::args().collect();
        match argv.get(1).map(String::as_str) {
            Some(platform::service::SERVICE_ARG) => return platform::service::run_service(),
            Some(platform::firewall::APPLY_ARG) => std::process::exit(if platform::firewall::apply_from_arg(&argv[2..]) { 0 } else { 1 }),
            Some(platform::secure::HELPER_ARG) => return platform::secure::run_helper(),
            Some(platform::micdriver::ARG) => return platform::micdriver::cli(&argv[2..]),
            _ => {}
        }
    }
    // Windows: the sign-in host (see signin.rs) is the host, minus its flag.
    #[cfg(windows)]
    let args = if std::env::args().nth(1).as_deref() == Some(signin::ARG) {
        signin::enable();
        platform::secure::mark_helper();
        Args::parse_from(std::env::args().enumerate().filter(|(i, _)| *i != 1).map(|(_, a)| a))
    } else {
        Args::parse()
    };
    #[cfg(not(windows))]
    let args = Args::parse();
    // Quick support goes through the background service when it's running here, so only
    // one process captures the screen. Otherwise it runs a host of its own, keeping
    // everything in a temporary config chosen before anything (or any thread) reads the
    // environment.
    let support_via_service =
        matches!(&args.command, Some(Command::Support { test: false, .. })) && support::service_running();
    let support_dir = match &args.command {
        Some(Command::Support { .. }) if !support_via_service => {
            let dir = support::new_dir()?;
            // SAFETY: still single-threaded; nothing has read the environment yet.
            unsafe { std::env::set_var("GLASS_CONFIG_DIR", &dir) };
            Some(dir)
        }
        _ => None,
    };
    let rt = tokio::runtime::Runtime::new()?;
    if let Some(Command::Support { no_window, json, test }) = &args.command {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let session = match support_dir {
            None => support::Session::start_via_service()?,
            Some(dir) => match support::Session::start(dir.clone(), *test) {
                Ok(s) => s,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
            },
        };
        // On the computer's own screen: a window. Over SSH, with --no-window or --json: here.
        #[cfg(any(target_os = "macos", windows))]
        if !no_window && !json && std::env::var_os("SSH_CONNECTION").is_none() {
            return app::run_support(rt, session);
        }
        let _ = no_window;
        return rt.block_on(support::run_terminal(session, *json));
    }
    // The app: `glass app`, Glass.app (whose executable is this binary), or the Start menu.
    #[cfg(any(target_os = "macos", windows))]
    match &args.command {
        Some(Command::App { install: true }) => {
            println!("{}", app::install_bundle()?.display());
            return Ok(());
        }
        Some(Command::App { install: false }) => return app::run(rt),
        _ => {}
    }
    // The host on macOS shows a menu bar icon (who's viewing), which needs the main thread:
    // the host itself then runs on the runtime's threads.
    #[cfg(target_os = "macos")]
    if args.command.is_none() && !args.test && std::env::var_os("GLASS_NO_MENUBAR").is_none() {
        let handle = rt.handle().clone();
        std::thread::spawn(move || {
            let result = rt.block_on(run_host(args));
            if let Err(e) = &result {
                eprintln!("Error: {e:#}");
            }
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        });
        platform::menubar::run(handle);
    }
    // The host on Windows shows a notification-area icon (who's viewing) on its own thread.
    #[cfg(windows)]
    // (Quick support's host sets GLASS_NO_MENUBAR, as on a Mac: its window is the UI.)
    if args.command.is_none() && !args.test && std::env::var_os("GLASS_NO_MENUBAR").is_none() && !signin::active() {
        platform::tray::start(rt.handle().clone());
    }
    rt.block_on(async_main(args))
}

async fn async_main(args: Args) -> Result<()> {
    match &args.command {
        None => run_host(args).await,
        Some(Command::Setup { yes: false, .. }) => setup::run().await,
        Some(Command::Setup { yes: true, json }) => setup::run_unattended(*json).await,
        Some(Command::Status { json: false }) => print_status(),
        Some(Command::Status { json: true }) => {
            let status = status::read().filter(|s| s.is_fresh());
            let problems = status.as_ref().map(status::Status::remote_problems);
            println!(
                "{}",
                serde_json::json!({
                    "running": status.is_some(),
                    "remote_ready": problems.as_ref().is_some_and(Vec::is_empty),
                    "remote_problems": problems.unwrap_or_else(|| vec!["Glass isn't running"]),
                    "status": status,
                })
            );
            Ok(())
        }
        Some(Command::Feedback { message }) => send_feedback(&message.join(" ")),
        Some(Command::Mcp { machine, url, token_file }) => mcp::run(url.clone(), machine.clone(), token_file.clone()),
        Some(Command::Agents { action, id, json }) => grants::cli(action, id.as_deref(), *json),
        Some(Command::Update) => update::run_cli(),
        Some(Command::Link { list: true, json, .. }) => list_passes(*json),
        Some(Command::Link {
            revoke: Some(id),
            json,
            ..
        }) if !id.is_empty() => {
            anyhow::ensure!(
                access::revoke(&access::file_for(&default_token_file()), id)?,
                "no single device matches {id}; see glass link --list"
            );
            if *json {
                println!("{}", serde_json::json!({ "revoked": 1 }));
            } else {
                println!("Revoked. That device lost access within seconds.");
            }
            Ok(())
        }
        Some(Command::Link {
            revoke: Some(_),
            json,
            ..
        }) => {
            let n = access::revoke_all(&access::file_for(&default_token_file()))?;
            if *json {
                println!("{}", serde_json::json!({ "revoked": n }));
            } else {
                println!(
                    "Revoked. {n} device{} signed in with a magic link lost access; unused links no longer work.",
                    if n == 1 { "" } else { "s" }
                );
            }
            Ok(())
        }
        Some(Command::Link {
            grant,
            json,
            anywhere,
            view_only,
            no_files,
            no_clipboard,
            ask,
            passkey,
            ..
        }) => print_link(
            *grant,
            *json,
            *anywhere,
            access::Scope {
                view_only: *view_only,
                files: !*no_files,
                clipboard: !*no_clipboard,
                ask: *ask,
                passkey: *passkey,
                support: false,
            },
        ),
        Some(Command::Config { key, value }) => config_cli(key.as_deref(), value.as_deref()),
        Some(Command::Acl { action, json }) => acl_cli(action.as_ref(), *json),
        Some(Command::Log { count, json }) => {
            let entries = connlog::recent(*count);
            if *json {
                println!("{}", serde_json::json!({ "connections": entries }));
            } else if entries.is_empty() {
                println!("No connections yet.");
            } else {
                for e in &entries {
                    println!("{}", connlog::describe(e));
                }
            }
            Ok(())
        }
        Some(Command::Viewers { json }) => {
            let reply = ipc::call(&ipc::Request::Viewers)?;
            if *json {
                println!("{reply}");
            } else if reply["viewers"].as_array().is_none_or(|v| v.is_empty()) {
                println!("Nobody is connected.");
            } else {
                for v in reply["viewers"].as_array().into_iter().flatten() {
                    println!("{}  {}", v["id"], v["describe"].as_str().unwrap_or(""));
                }
            }
            Ok(())
        }
        Some(Command::Kick { id }) => {
            let reply = ipc::call(&ipc::Request::Disconnect { id: *id })?;
            println!("Disconnected {}.", reply["disconnected"]);
            Ok(())
        }
        Some(Command::Requests { json }) => {
            let reply = ipc::call(&ipc::Request::Pending)?;
            if *json {
                println!("{reply}");
            } else if reply["pending"].as_array().is_none_or(|p| p.is_empty()) {
                println!("Nobody is waiting to connect.");
            } else {
                for r in reply["pending"].as_array().into_iter().flatten() {
                    println!("{}  {}", r["id"], r["who"].as_str().unwrap_or(""));
                }
                println!("\nglass allow <id> · glass deny <id>");
            }
            Ok(())
        }
        Some(Command::Allow { id }) => decide_cli(*id, true),
        Some(Command::Deny { id }) => decide_cli(*id, false),
        #[cfg(any(target_os = "macos", windows))]
        Some(Command::Mic { action }) => {
            match action.as_str() {
                "install" => println!("{}", mic::install(mic::Ask::Terminal)?),
                "uninstall" => println!("{}", mic::uninstall(mic::Ask::Terminal)?),
                _ => match mic::device_name() {
                    Some(name) if cfg!(target_os = "macos") => {
                        println!("Ready: a viewer's microphone plays into {name}, which is the Mac's default input while it's on.")
                    }
                    Some(name) => println!("Ready: while a viewer's mic is on, apps here can pick \"{name}\"."),
                    None if cfg!(target_os = "macos") => {
                        println!("Not set up: a viewer's microphone needs Glass Microphone, a small audio driver. Install it: glass mic install")
                    }
                    None if cfg!(windows) => println!(
                        "Not set up: a viewer's microphone needs VB-CABLE, a free virtual audio cable (https://vb-audio.com/Cable/). Get it: glass mic install"
                    ),
                    None => println!("Not set up: a viewer's microphone needs pactl and PipeWire or PulseAudio. Set it up: glass mic install"),
                },
            }
            Ok(())
        }
        Some(Command::Chat { message }) => {
            let reply = ipc::call(&ipc::Request::Chat {
                text: message.join(" "),
            })?;
            println!("Sent to {} viewer(s).", reply["sent"]);
            Ok(())
        }
        Some(Command::Notifications { action, id }) => {
            match action.as_str() {
                "test" => println!("Sent to {} device(s).", push::test(id.as_deref())?),
                "off" => {
                    let id = id.as_deref().context("which device? `glass notifications` lists them")?;
                    anyhow::ensure!(push::remove(id)?, "no device {id}");
                    println!("Notifications off for {id}.");
                }
                _ => {
                    let subs = push::list();
                    if subs.is_empty() {
                        println!("No devices get notifications. Turn them on in a viewer: the bell in its toolbar.");
                    }
                    for s in subs {
                        let kinds = [(s.kinds.chat, "messages"), (s.owner && s.kinds.asks, "questions"), (s.owner && s.kinds.activity, "activity")]
                            .into_iter()
                            .filter_map(|(on, k)| on.then_some(k))
                            .collect::<Vec<_>>()
                            .join(", ");
                        println!("{}  {}{}  {}", s.id, s.device, if s.owner { "" } else { " (guest)" }, kinds);
                    }
                }
            }
            Ok(())
        }
        Some(Command::Privacy { state }) => {
            ipc::call(&ipc::Request::Privacy { on: state == "on" })?;
            println!("Privacy mode {state} (until everyone disconnects).");
            Ok(())
        }
        Some(Command::Wake { target }) => {
            let macs = wol::wake_target(target)?;
            println!("Sent a wake-up to {target} ({macs}).");
            Ok(())
        }
        #[cfg(windows)]
        Some(Command::Service { action, remove_host_rule, firewall_scope }) => {
            platform::service::cli(action, *remove_host_rule, firewall_scope.as_deref())
        }
        Some(Command::Remote { action }) => remote_cli(action),
        Some(Command::Account { action, json }) => account_cli(action, *json),
        Some(Command::Fleet { action, arg, json }) => fleet_cli(action, arg.as_deref(), *json),
        #[cfg(target_os = "macos")]
        Some(Command::DisplayTest { size, seconds }) => {
            let (w, h) = size
                .split_once(['x', '×'])
                .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                .context("size like 2048x1536")?;
            platform::virtual_display::self_test(w, h, *seconds)
        }
        Some(Command::Token { set: true, .. }) => set_token(&default_token_file()),
        Some(Command::Token { reset, .. }) => {
            let path = default_token_file();
            if *reset {
                let _ = std::fs::remove_file(&path);
            }
            let token = control::load_or_create_token(&path)?;
            println!("{token}");
            if *reset {
                eprintln!(
                    "New token saved. The running host uses it for new connections right away."
                );
            }
            Ok(())
        }
        Some(Command::Restart) => restart_service(),
        #[cfg(any(target_os = "macos", windows))]
        Some(Command::App { .. }) => unreachable!("handled before the runtime starts"),
        Some(Command::Support { .. }) => unreachable!("handled before the runtime starts"),
        Some(Command::Uninstall { everything }) => {
            // Published to the internet for agents: not any more.
            if grants::load().public
                && let Err(e) = oauth::publish(false)
            {
                eprintln!("Couldn't stop publishing to agents with Tailscale Funnel ({e:#}); run: tailscale funnel --https=8443 off");
            }
            agent::uninstall()?;
            #[cfg(windows)]
            if *everything {
                platform::remove::everything()?;
                println!("Glass is removed. Its settings stay in %APPDATA%\\Glass, in case you install it again.");
                return Ok(());
            }
            #[cfg(not(windows))]
            let _ = everything;
            if cfg!(windows) {
                println!(
                    "Glass service removed. Delete %LOCALAPPDATA%\\Programs\\Glass and %APPDATA%\\Glass to remove everything."
                );
            } else {
                println!("Glass service removed. Delete ~/.glass and ~/.config/glass to remove everything.");
            }
            Ok(())
        }
    }
}

fn print_status() -> Result<()> {
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    match status::read().filter(|s| s.is_fresh()) {
        Some(s) => {
            println!("Glass {} running (pid {})", s.version, s.pid);
            println!("  capturing:        {}", yes_no(s.capturing));
            println!("  screen recording: {}", yes_no(s.screen_recording));
            println!("  accessibility:    {}", yes_no(s.accessibility));
            let remote = match s.remote.as_str() {
                "ready" => "ready (glass link --anywhere makes a link)",
                "connecting" => "connecting to glassdesk.dev…",
                _ => "off (glass remote on)",
            };
            println!("  remote:           {remote}");
            if cfg!(windows) {
                let pads = if gamepad::supported() { "yes (ViGEmBus)".to_string() } else { format!("no: install ViGEmBus ({})", gamepad::VIGEM_DOWNLOAD) };
                println!("  controllers:      {pads}");
            }
            if let Some(url) = &s.url {
                println!("  open:             {url}");
            }
            let problems = s.remote_problems();
            if problems.is_empty() {
                println!("Ready to be reached from anywhere: yes");
            } else {
                println!("Ready to be reached from anywhere: no, {}", problems.join("; "));
            }
        }
        None => println!(
            "Glass isn't running.{}",
            if agent::is_installed() {
                agent::LOG_HINT
            } else {
                " Run `glass setup`."
            }
        ),
    }
    Ok(())
}

fn send_feedback(message: &str) -> Result<()> {
    anyhow::ensure!(
        !message.trim().is_empty(),
        "usage: glass feedback \"what happened or what you'd like\""
    );
    post_feedback(message, "cli")?;
    println!("Thanks! Feedback sent.");
    Ok(())
}

pub(crate) fn post_feedback(message: &str, source: &str) -> Result<()> {
    let body = serde_json::json!({
        "message": message,
        "source": source,
        "version": env!("CARGO_PKG_VERSION"),
        "os": platform::os_version(),
    });
    let status = std::process::Command::new("curl")
        .args([
            "-fsS",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "--data-binary",
        ])
        .arg(body.to_string())
        .arg(FEEDBACK_URL)
        .stdout(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "couldn't reach {FEEDBACK_URL}");
    Ok(())
}

async fn run_host(args: Args) -> Result<()> {
    let paths = Paths::resolve(args.web_dir.clone(), args.log_dir.clone());
    std::fs::create_dir_all(&paths.log_dir)?;
    let host_log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.log_dir.join("host.log"))?;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_ansi(false)
        .with_writer(std::io::stdout.and(Mutex::new(host_log)))
        .init();
    // wtransport and axum-server pull in different rustls crypto backends; pick one.
    let _ = rustls::crypto::ring::default_provider().install_default();

    if args.test {
        testmode::enable();
        info!("TEST MODE: test pattern, recorded input, in-memory clipboard");
    }
    if support::is_host() {
        info!("quick support session: {}", config_dir().display());
        support::watch_parent();
    }
    if let Some(dir) = &args.downloads_dir {
        files::set_downloads_dir(dir.clone());
    }

    let (frames, _) = broadcast::channel(8);
    // ~80 ms of 5 ms PCM packets and the 10 ms Opus packets made from them; a session that
    // falls further behind just skips ahead.
    let (audio, _) = broadcast::channel(24);
    // One Opus encoder for every viewer that takes it, whichever source fills the channel.
    opus_audio::start(audio.clone());
    let (events, _) = broadcast::channel(32);

    // Windows: link to the SYSTEM secure-desktop helper if the service is installed, so
    // viewers can use the lock screen, answer UAC, control elevated windows and Ctrl+Alt+Del.
    // A no-op (degrading to today's behaviour) when it isn't.
    // (The sign-in host is SYSTEM itself, on the sign-in screen: it needs no helper.)
    #[cfg(windows)]
    if !args.test && !signin::active() {
        platform::secure::start(frames.clone());
    }
    if signin::active() {
        info!("sign-in host: the sign-in screen, until someone signs in (screen, keyboard and mouse only)");
    }

    let token = control::load_or_create_token(&args.token_file)?;
    let tls = match args.cert.clone().zip(args.key.clone()) {
        Some((cert, key)) => Some(server::TlsFiles { cert, key }),
        None if args.test => None,
        None => tailscale::ensure_cert(&config_dir().join("certs"))
            .await
            .map(|(cert, key)| server::TlsFiles { cert, key }),
    };
    let host = tls
        .as_ref()
        .and_then(|t| t.cert.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "localhost".into());
    let scheme = if tls.is_some() { "https" } else { "http" };
    let listen = listen::Listen::resolve(&args.bind, &settings::load().bind).await?;
    info!("listening on {}", listen::describe(&listen));
    // The standard HTTPS port, if we can have it: then URLs carry no port at all.
    let https_listeners = if tls.is_some() {
        listen::bind_https(&listen, args.https_port)?
    } else {
        Vec::new()
    };
    // The usual web port, or the next free one, on every listen address.
    let http_listeners = listen::bind_tcp(&listen, args.http_port)?;
    let web_port = http_listeners[0].local_addr()?.port();
    if web_port != args.http_port {
        tracing::warn!(
            "port {} is in use; serving the web client on {web_port}",
            args.http_port
        );
    }
    // `tailscale serve` (Linux, when 443 isn't available) forwards to localhost. That only
    // reaches Glass when the listen list includes IPv4 loopback, which the default does.
    let via_serve = tls.is_some() && tailscale::serves_https_to(web_port).await;
    if via_serve && !listen.covers_ipv4_loopback() {
        tracing::warn!(
            "tailscale serve forwards to localhost:{web_port}, but that address isn't being listened on. Add 127.0.0.1 to --bind so those URLs connect."
        );
    }
    let portless = !https_listeners.is_empty() || (via_serve && listen.covers_ipv4_loopback());
    let url = if portless {
        format!("{scheme}://{host}/")
    } else {
        format!("{scheme}://{host}:{web_port}/")
    };
    status::start(Some(url.clone()));

    let (video, injector, clipboard, tailnet_owner, _awake): (
        Arc<dyn media::VideoSource>,
        Box<dyn media::InputSink>,
        Arc<dyn media::Clipboard>,
        _,
        _,
    ) = if args.test {
        let video = Arc::new(testmode::TestVideo::new(
            frames.clone(),
            args.fps as u32,
            (args.mbps * 1_000_000) as u32,
        ));
        video.set_layout(media::Layout::Single(None))?;
        testmode::start_tone(audio.clone());
        (
            video,
            Box::new(testmode::FakeInput),
            Arc::new(testmode::FakeClipboard),
            // Who owns the computer on Tailscale, when a test says. Never polls Tailscale.
            tailscale::TailnetOwner::fixed(std::env::var("GLASS_TEST_TAILNET_OWNER").ok()),
            None,
        )
    } else {
        // Keep displays (and the system) awake for as long as the host runs.
        let awake = platform::keep_awake();
        lid::watch();
        if cfg!(windows) && !gamepad::supported() {
            info!("game controllers: off until ViGEmBus is installed ({})", gamepad::VIGEM_DOWNLOAD);
        }
        let video = Arc::new(video::Video::new(
            video::VideoConfig {
                // Linux encodes in software: 60 at most. Windows caps OpenH264 at 60 itself
                // and lets its hardware encoder go faster.
                fps: if cfg!(target_os = "linux") { args.fps.min(60) } else { args.fps },
                bitrate: args.mbps * 1_000_000,
                scale: args.scale,
            },
            frames.clone(),
            audio.clone(),
        ));
        if input::has_accessibility() {
            info!("accessibility: granted (input injection enabled)");
        } else {
            tracing::warn!(
                "accessibility: NOT granted; remote input will be ignored. Allow it in {}",
                platform::setup::input_settings()
            );
        }
        permissions::request_missing();
        let display = loop {
            match video.set_layout(media::Layout::Single(None)) {
                Ok(streams) => break streams[0].clone(),
                // On macOS a missing Screen Recording grant isn't a sleeping display: capture
                // stays blocked until the user allows it, so retry slowly instead.
                Err(e) if cfg!(target_os = "macos") && !permissions::has_screen_recording() => {
                    tracing::warn!(
                        "{e:#}; Screen Recording isn't allowed. Run `glass setup`, or allow Glass \
                         in {}",
                        platform::setup::screen_settings()
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                }
                Err(e) => {
                    tracing::warn!("{e:#}; waking displays and retrying");
                    platform::wake_displays();
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        };
        if !signin::active() {
            clipboard::watch(events.clone());
        }
        // A miss (Tailscale not up yet) is resolved on the next connection, then kept.
        let tailnet_owner = tailscale::TailnetOwner::unknown();
        match tailscale::self_login().await {
            Ok(login) => {
                info!("tailnet devices owned by {login} connect without a token");
                tailnet_owner.store(login);
            }
            Err(e) => info!("tailscale unavailable ({e:#}); will check again on connection"),
        }
        (
            video,
            Box::new(input::Injector::new(display.id)),
            Arc::new(clipboard::SystemClipboard),
            tailnet_owner,
            awake,
        )
    };
    status::set_capturing(true);
    // The sign-in host neither updates itself nor takes CLI commands (the user's host,
    // started at sign-in, does).
    if !signin::active() {
        update::watch();
        if let Err(e) = ipc::serve() {
            tracing::warn!("CLI socket: {e:#}");
        }
    }

    let test_routes = args
        .test
        .then(|| testmode::routes(events.clone(), files::downloads_dir()));
    let shared = Arc::new(server::Shared {
        frames,
        injector: Mutex::new(injector),
        video,
        clipboard,
        events,
        token_file: args.token_file.clone(),
        auth_failures: Mutex::default(),
        tailnet_owner,
        audio,
        rate: Mutex::new(rate::RateControl::new((args.mbps * 1_000_000) as u32)),
    });
    // Once a second: restart any capture that ended, and re-share the bitrate between
    // displays by activity.
    {
        let video = shared.video.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tick.tick().await;
                let v = video.clone();
                let _ = tokio::task::spawn_blocking(move || v.heal()).await;
                video.rebalance();
            }
        });
    }
    info!("open {url}#t={token}");
    server::run(
        shared,
        paths.web_dir,
        args.http_port,
        http_listeners,
        https_listeners,
        listen,
        args.wt_port,
        args.wt_host,
        tls,
        paths.log_dir.join("client.log"),
        test_routes,
    )
    .await
}

/// How many ports past the usual one to try when it's taken.
pub const PORT_TRIES: u16 = 20;

pub fn home() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// ~/.config/glass (%APPDATA%\Glass on Windows), or $GLASS_CONFIG_DIR (tests use it to
/// stay out of the real config).
pub fn config_dir() -> PathBuf {
    std::env::var_os("GLASS_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            cfg!(windows)
                .then(|| std::env::var_os("APPDATA"))
                .flatten()
                .map(|d| PathBuf::from(d).join("Glass"))
        })
        .unwrap_or_else(|| home().join(".config/glass"))
}

pub fn default_token_file() -> PathBuf {
    config_dir().join("token")
}

struct Paths {
    web_dir: PathBuf,
    log_dir: PathBuf,
}

impl Paths {
    /// Installed layout is `<prefix>/bin/glass` + `<prefix>/share/glass/web`; otherwise
    /// fall back to the source tree this binary was built from.
    fn resolve(web_dir: Option<PathBuf>, log_dir: Option<PathBuf>) -> Self {
        let installed = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                Some(
                    exe.canonicalize()
                        .ok()?
                        .parent()?
                        .parent()?
                        .join("share/glass/web"),
                )
            })
            .filter(|dir| dir.is_dir());
        let source = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."));
        let web_dir = web_dir
            .or(installed.clone())
            .unwrap_or_else(|| source.join("web"));
        let log_dir = log_dir.unwrap_or_else(|| match installed {
            Some(_) => platform::log_dir(),
            None => source.join("logs"),
        });
        Self { web_dir, log_dir }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_flag_is_repeatable_and_comma_separated() {
        let args = Args::try_parse_from([
            "glass",
            "--bind",
            "127.0.0.1,::1",
            "--bind",
            "Tailscale",
        ])
        .unwrap();
        assert_eq!(
            listen::parse_bind(&args.bind).unwrap(),
            vec!["127.0.0.1", "::1", "tailscale"]
        );
    }

    #[test]
    fn saved_bind_applies_when_the_flag_is_absent() {
        let args = Args::try_parse_from(["glass"]).unwrap();
        assert!(args.bind.is_empty());
        let saved = vec!["100.64.0.2".to_string()];
        assert_eq!(
            listen::parse_bind(if args.bind.is_empty() { &saved } else { &args.bind }).unwrap(),
            vec!["100.64.0.2"]
        );
    }
}
