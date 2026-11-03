//! Windows-specific setup: letting Glass through Windows Defender Firewall.

use anyhow::Result;
use cliclack::{confirm, log, note};

use super::firewall::{self, State};
use super::firewall_scope::RemoteScope;
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
    match super::service::cli("install") {
        Ok(()) if matches!(super::service::status(), Ok(Some(_))) => {
            log::success("The secure-desktop service is installed")?;
        }
        _ => {
            log::warning("The service wasn't installed. Try again as administrator: glass service install")?;
        }
    }
    Ok(())
}

/// Without a rule, Windows asks the first time the host listens, on this screen, and
/// "Cancel" there leaves Glass blocked. Adding or narrowing the rule asks once, here.
fn firewall_step() -> Result<()> {
    log::step("Windows Defender Firewall")?;
    let views = firewall::inspect();
    let wanted = firewall::scope();
    if views.is_empty() || views.iter().all(|view| view.state == State::Unreadable) {
        log::warning("Couldn't read the firewall's rules; Windows may ask about Glass when it starts.")?;
        return Ok(());
    }
    if views.iter().all(|view| view.state == State::Ready) {
        log::success(firewall::ready_sentence(wanted))?;
        if views.iter().any(|view| view.foreign_wider) {
            log::warning(firewall::foreign_warning(wanted))?;
        }
        return Ok(());
    }
    if views.iter().all(|view| view.state == State::Blocked) {
        log::warning(firewall::blocked_hint())?;
        return Ok(());
    }
    if !views.iter().any(|view| view.state == State::NeedsChange) {
        if views.iter().any(|view| view.state == State::Blocked) {
            log::warning(firewall::blocked_hint())?;
        }
        if views.iter().any(|view| view.state == State::Unreadable) {
            log::warning("Couldn't read the firewall's rules; Windows may ask about Glass when it starts.")?;
        }
        return Ok(());
    }
    let updating = views.iter().any(|view| view.has_glass_allow);
    match wanted {
        RemoteScope::Tailscale => note(
            "Only Tailscale can connect",
            "Glass is set to listen on Tailscale, so its firewall rule should allow only\n\
             Tailscale addresses (100.64.0.0/10 and fd7a:115c:a1e0::/48), not the rest of\n\
             the local network, and should block edge traversal. Windows asks for\n\
             administrator approval once.",
        )?,
        RemoteScope::Loopback => note(
            "No inbound rule is needed",
            "Glass is set to listen on this computer only. The firewall doesn't filter\n\
             loopback, so Glass will remove its inbound allow rule. Windows asks for\n\
             administrator approval once.",
        )?,
        RemoteScope::Any => note(
            "Let your other devices connect",
            "Windows Defender Firewall blocks incoming connections until a program is allowed.\n\
             Glass can add a rule for itself on private networks (home, work and Tailscale),\n\
             not public ones like a café's. Windows asks for administrator approval once.",
        )?,
    }
    let question = if updating { "Update the firewall rule now?" } else { "Add the rule now?" };
    if !confirm(question).initial_value(true).interact()? {
        if wanted == RemoteScope::Any && !updating {
            log::warning("Skipped: Windows will ask when Glass first starts. Choose Allow there.")?;
        } else {
            log::warning("Skipped. The firewall rule is unchanged.")?;
        }
        return Ok(());
    }
    let after = firewall::elevate_sync();
    let pending: Vec<_> = after.iter().filter(|view| matches!(view.state, State::NeedsChange | State::Unreadable)).collect();
    let blocked = after.iter().any(|view| view.state == State::Blocked);
    if pending.is_empty() && !blocked {
        log::success(firewall::ready_sentence(wanted))?;
    } else {
        for view in &pending {
            let detail = if view.detail.is_empty() {
                format!("the firewall rule wasn't updated.\n{}", firewall::manual_command(&view.path, wanted))
            } else {
                view.detail.clone()
            };
            log::warning(format!("{}: {detail}", view.path.display()))?;
        }
        if blocked {
            log::warning(firewall::blocked_hint())?;
        }
    }
    if after.iter().any(|view| view.foreign_wider) {
        log::warning(firewall::foreign_warning(wanted))?;
    }
    Ok(())
}

/// Updates the firewall rule when already running as administrator; reports it otherwise.
/// No extra approval prompt: unattended setup only says what to run.
pub fn prepare_unattended() -> Vec<crate::setup::Need> {
    let mut needs = Vec::new();
    let views = if super::service::is_elevated() { firewall::sync() } else { firewall::inspect() };
    let wanted = firewall::scope();
    if views.iter().any(|view| view.state == State::NeedsChange) {
        let commands = views
            .iter()
            .filter(|view| view.state == State::NeedsChange)
            .map(|view| manual_lines(view, wanted))
            .collect::<Vec<_>>()
            .join("\n");
        let action = match wanted {
            RemoteScope::Tailscale => format!(
                "Allow Glass only from Tailscale (100.64.0.0/10 and fd7a:115c:a1e0::/48) on private networks, and block edge traversal. Run `glass setup` (asks for administrator approval once), or as administrator:\n{commands}"
            ),
            RemoteScope::Loopback => format!(
                "Glass listens on this computer only, so it should have no inbound firewall rule. Run `glass setup`, or as administrator:\n{commands}"
            ),
            RemoteScope::Any => format!(
                "Allow Glass through Windows Defender Firewall on private networks: run `glass setup` on the computer (asks for administrator approval once), or as administrator:\n{commands}"
            ),
        };
        needs.push(crate::setup::Need { step: "firewall", action, on_screen: false });
    } else if views.iter().any(|view| view.state == State::Blocked) {
        needs.push(crate::setup::Need {
            step: "firewall",
            action: "Windows Defender Firewall blocks Glass: in Windows Security › Firewall & network protection › Allow an app through firewall, tick Private for Glass".into(),
            on_screen: false,
        });
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

/// Commands to print for one program. A fresh default install only needs the add;
/// replacing a rule Glass already created needs the delete as well.
fn manual_lines(view: &firewall::ProgramView, scope: RemoteScope) -> String {
    let command = if scope == RemoteScope::Any && !view.has_glass_allow {
        format!("netsh {}", firewall::add_rule_args(&view.path, scope))
    } else {
        firewall::manual_command(&view.path, scope)
    };
    command.lines().map(|line| format!("  {line}")).collect::<Vec<_>>().join("\n")
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
