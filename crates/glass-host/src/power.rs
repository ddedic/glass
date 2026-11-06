//! Restarting the computer, or just Glass, from the viewer. The viewer keeps retrying, so
//! it reconnects once Glass is back (after login, for a computer restart).

use anyhow::Result;

pub fn restart_glass() -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_restart("glass");
        return Ok(());
    }
    tracing::info!("restarting Glass (asked by a viewer)");
    exit_to_restart();
    Ok(())
}

/// The certificate arrived after startup (`tailscale::wait_for_cert`). The listeners, the
/// viewer's address and WebTransport are all chosen once at startup, so start again.
pub fn restart_for_certificate() {
    tracing::info!("restarting Glass (Tailscale is up: serving HTTPS)");
    exit_to_restart();
}

fn exit_to_restart() {
    crate::viewers::disconnect_all();
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        // A non-zero exit: the login item / systemd unit starts it again.
        std::process::exit(75);
    });
}

pub fn restart_computer() -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_restart("computer");
        return Ok(());
    }
    tracing::info!("restarting the computer (asked by a viewer)");
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("osascript")
        .args(["-e", "tell application \"loginwindow\" to «event aevtrrst»"])
        .status()?;
    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("systemctl").arg("reboot").status()?;
    #[cfg(windows)]
    let status = std::process::Command::new("shutdown").args(["/r", "/t", "0"]).status()?;
    anyhow::ensure!(status.success(), "the system didn't accept the restart");
    Ok(())
}
