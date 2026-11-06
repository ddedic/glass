//! Runs the host as a systemd user service tied to the graphical session.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

pub const UNIT: &str = "glass.service";
pub const LOG_HINT: &str = " Check `journalctl --user -u glass`.";

/// Session variables the host needs to reach Wayland, the portal and PipeWire.
const SESSION_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_TYPE",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
];

pub fn unit_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home().join(".config"))
        .join("systemd/user")
        .join(UNIT)
}

pub fn is_installed() -> bool {
    unit_path().exists()
}

fn systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("systemctl")?;
    anyhow::ensure!(
        status.success(),
        "systemctl --user {} failed",
        args.join(" ")
    );
    Ok(())
}

/// Writes the unit for this binary and (re)starts it.
pub fn install(start_at_login: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let unit = format!(
        "[Unit]\n\
         Description=Glass remote desktop host\n\
         After=graphical-session.target pipewire.service\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         Environment=GLASS_SUPERVISED=1\n\
         ExecStart={exe}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n",
        exe = exe.display(),
    );
    std::fs::create_dir_all(unit_path().parent().unwrap())?;
    std::fs::write(unit_path(), unit)?;

    // Most sessions (uwsm on Omarchy) already export these; importing is harmless if so.
    let present: Vec<&str> = SESSION_ENV
        .iter()
        .copied()
        .filter(|v| std::env::var_os(v).is_some())
        .collect();
    if !present.is_empty() {
        let mut args = vec!["import-environment"];
        args.extend(present);
        let _ = systemctl(&args);
    }
    systemctl(&["daemon-reload"])?;
    if start_at_login {
        systemctl(&["enable", UNIT])?;
    } else {
        let _ = systemctl(&["disable", UNIT]);
    }
    restart()
}

pub fn restart() -> Result<()> {
    systemctl(&["restart", UNIT])
}

pub fn uninstall() -> Result<()> {
    let _ = systemctl(&["disable", "--now", UNIT]);
    if unit_path().exists() {
        std::fs::remove_file(unit_path())?;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(())
}
