//! Windows-specific setup: letting Glass through Windows Defender Firewall.

use anyhow::Result;
use cliclack::{confirm, log, note};

use super::firewall::{self, Inbound, Remote};
use crate::status::Status;

pub async fn prepare() -> Result<()> {
    firewall_step()?;
    service_step()
}

/// Offers to install the secure-desktop service, so viewers can use the lock screen, answer
/// UAC prompts, control elevated windows and send Ctrl+Alt+Del. Optional and separate from
/// the per-user host, which keeps working either way.
fn service_step() -> Result<()> {
    log::step("Secure desktop (lock screen, UAC, Ctrl+Alt+Del)")?;
    match super::service::status() {
        Ok(Some(_)) => {
            log::success("The Glass secure-desktop service is installed")?;
            return Ok(());
        }
        Ok(None) => {}
        Err(_) => {
            log::warning("Couldn't check the secure-desktop service.")?;
            return Ok(());
        }
    }
    note(
        "Use the computer even when it's locked",
        "Without this, a viewer can't see or use the lock screen or sign-in screen, answer a\n\
         Windows security (UAC) prompt, control a program running as administrator, or send\n\
         Ctrl+Alt+Del. A small background service (running as the system) handles those.\n\
         It never accepts connections itself — your Glass host stays the only way in.\n\
         Windows asks for administrator approval once.",
    )?;
    if !confirm("Install the secure-desktop service?").initial_value(true).interact()? {
        log::warning("Skipped. Install it later with: glass service install")?;
        return Ok(());
    }
    match super::service::cli("install", false, None) {
        Ok(()) if matches!(super::service::status(), Ok(Some(_))) => {
            log::success("The secure-desktop service is installed")?;
        }
        _ => {
            log::warning("The service wasn't installed. Try again as administrator: glass service install")?;
        }
    }
    Ok(())
}

fn warn_other_allows(rules: &[firewall::Rule]) -> Result<()> {
    let names = firewall::other_allow_names(rules);
    if names.is_empty() {
        return Ok(());
    }
    let which = if names.len() == 1 { "another rule" } else { "other rules" };
    log::warning(format!("Windows also allows this program through {which}: {}.", names.join(", ")))?;
    Ok(())
}

/// Without a rule, Windows asks the first time the host listens, on this screen, and
/// "Cancel" there leaves Glass blocked. Adding the rule now asks once, here, with context.
fn firewall_step() -> Result<()> {
    log::step("Windows Defender Firewall")?;
    let exe = std::env::current_exe()?;
    let remote = match firewall::saved_remote() {
        Ok(remote) => remote,
        Err(()) => {
            log::warning("Couldn't read the saved listen address, so no firewall rule was added.")?;
            return Ok(());
        }
    };
    // One listing for the block check, the in-sync check, and the missing check.
    let Some(listed) = firewall::program_rules(&exe) else {
        log::warning("Couldn't read the firewall's rules; Windows may ask about Glass when it starts.")?;
        return Ok(());
    };
    // Before any skip, failure, or success line, so other allows are named either way.
    warn_other_allows(&listed)?;
    let state = firewall::decide(&listed, |_| true);
    // A loopback bind needs no inbound allow, so a block rule is not something to tick Private for.
    if state == Inbound::Blocked && remote != Remote::Loopback {
        log::warning(
            "Windows Defender Firewall is blocking Glass (maybe its prompt was turned down). In\n\
             Windows Security › Firewall & network protection › Allow an app through firewall,\n\
             tick Private for Glass, then run `glass setup` again.",
        )?;
        return Ok(());
    }
    if firewall::glass_matches(&listed, remote) {
        log::success(firewall::ready_sentence(remote))?;
        return Ok(());
    }
    let missing = state == Inbound::Missing;
    let (title, body, question, skip) = match (remote, missing) {
        (Remote::Loopback, _) => ("Glass only listens on this computer", "It doesn't need an inbound firewall rule, so Glass can remove the one named Glass for this program. Windows asks for administrator approval once.", "Remove the inbound rule now?", "Skipped: the firewall rule was left in place."),
        (Remote::Tailscale, _) => ("Limit Glass's own rule to Tailscale", "Glass's own firewall rule can allow only 100.64.0.0/10 and fd7a:115c:a1e0::/48. Any other allow for this program is left as it is. Windows asks for administrator approval once.", "Update Glass's firewall rule now?", "Skipped: Glass's firewall rule was left as it is."),
        (Remote::Any, true) => ("Let your other devices connect", "Windows Defender Firewall blocks incoming connections until a program is allowed.\nGlass can add a rule for itself on private networks (home, work and Tailscale),\nnot public ones like a café's. Windows asks for administrator approval once.", "Add the rule now?", "Skipped: Windows will ask when Glass first starts. Choose Allow there."),
        (Remote::Any, false) => ("Let your other devices connect", "Glass can update its firewall rule so private networks (home, work and Tailscale) can connect, not public ones like a café's. Windows asks for administrator approval once.", "Update the firewall rule now?", "Skipped: the firewall rule was left as it is."),
    };
    note(title, body)?;
    if !confirm(question).initial_value(true).interact()? {
        log::warning(skip)?;
        return Ok(());
    }
    let _ = firewall::add_rule_elevated(&exe);
    if firewall::in_sync(&exe, remote) == Some(true) {
        log::success(firewall::ready_sentence(remote))?;
    } else {
        log::warning("The firewall rule wasn't updated. Run `glass setup` again.")?;
    }
    Ok(())
}

/// Updates the firewall rule when already running as administrator; reports it otherwise.
pub fn prepare_unattended() -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    let Ok(exe) = std::env::current_exe() else { return needs };
    let action = match firewall::saved_remote() {
        Err(()) => Some("Couldn't read the saved listen address, so no firewall rule was added.".into()),
        Ok(remote) if remote != Remote::Loopback && firewall::inbound(&exe) == Some(Inbound::Blocked) => Some(
            "Windows Defender Firewall blocks Glass: in Windows Security › Firewall & network protection › Allow an app through firewall, tick Private for Glass".into(),
        ),
        Ok(remote) if firewall::in_sync(&exe, remote) == Some(true) => None,
        Ok(remote) if firewall::add_rule_quietly(&exe) && firewall::in_sync(&exe, remote) == Some(true) => None,
        Ok(remote) => Some(match remote {
            Remote::Tailscale => "Limit Glass's own firewall rule to Tailscale (100.64.0.0/10 and fd7a:115c:a1e0::/48). Run `glass setup`.".into(),
            Remote::Loopback => "Remove Glass's inbound firewall rule (it only listens on this computer). Run `glass setup`.".into(),
            Remote::Any => "Allow Glass through Windows Defender Firewall on private networks. Run `glass setup` (asks for administrator approval once).".into(),
        }),
    };
    if let Some(action) = action {
        needs.push(crate::setup::Need { step: "firewall", action, on_screen: false });
    }
    if matches!(super::service::status(), Ok(None)) {
        needs.push(crate::setup::Need {
            step: "secure-desktop",
            action: "Optional: to use the lock screen, answer UAC, control elevated apps and send Ctrl+Alt+Del, install the secure-desktop service (asks for administrator once): glass service install".into(),
            on_screen: false,
        });
    }
    needs
}

pub const TAILSCALE_INSTALL: &str = "Install Tailscale: winget install Tailscale.Tailscale (or https://tailscale.com/download/windows),\n\
     then sign in from its tray icon. Install it on your other devices too.";
pub const TAILSCALE_START: &str = "Tailscale is installed but its service isn't running. Start it from the Start menu, or run:\n  net start Tailscale";
pub const TAILSCALE_SIGN_IN: &str =
    "Tailscale isn't signed in. Click its tray icon and log in, or run: tailscale up";

pub fn open_url(url: &str) {
    // `start` treats its first quoted argument as a title, hence the empty one.
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    super::no_window(&mut cmd);
    let _ = cmd.status();
}

/// Where to allow screen capture, for messages.
pub fn screen_settings() -> String {
    "nothing (Windows doesn't ask); make sure someone is signed in at the computer".into()
}

/// Where to allow remote input, for messages.
pub fn input_settings() -> String {
    "nothing (Windows doesn't ask)".into()
}

/// Tailscale on Windows lets any local user run its CLI.
pub fn try_tailscale_operator() -> bool {
    false
}

pub fn permission_needs(status: &Status) -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    if !status.capturing {
        needs.push(crate::setup::Need {
            step: "screen",
            action: "Sign in at the computer: Glass captures the signed-in desktop".into(),
            on_screen: true,
        });
    }
    needs
}

pub fn permissions(status: Status) -> Result<()> {
    let _ = status;
    Ok(())
}
