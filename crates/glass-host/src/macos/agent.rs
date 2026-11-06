//! Runs the host as a per-user LaunchAgent: starts at login, restarts if it exits, and
//! gets its own Screen Recording / Accessibility grants (separate from the Terminal's).

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};

pub const LABEL: &str = "dev.glassdesk.host";
pub const LOG_HINT: &str = " Check ~/Library/Logs/Glass/agent.log.";

pub fn plist_path() -> PathBuf {
    crate::home()
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc_getuid() })
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

pub fn is_installed() -> bool {
    plist_path().exists()
}

/// Writes the agent plist for this binary and (re)starts it.
pub fn install(start_at_login: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let logs = super::log_dir();
    std::fs::create_dir_all(&logs)?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string></array>
  <key>RunAtLoad</key><{run_at_load}/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>EnvironmentVariables</key><dict>
    <key>GLASS_SUPERVISED</key><string>1</string>
    <key>PATH</key><string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
  </dict>
  <key>StandardOutPath</key><string>{out}</string>
  <key>StandardErrorPath</key><string>{out}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        run_at_load = if start_at_login { "true" } else { "false" },
        out = logs.join("agent.log").display(),
    );
    std::fs::create_dir_all(plist_path().parent().unwrap())?;
    std::fs::write(plist_path(), plist)?;

    stop();
    let status = Command::new("launchctl")
        .arg("bootstrap")
        .arg(domain())
        .arg(plist_path())
        .status()?;
    anyhow::ensure!(status.success(), "launchctl bootstrap failed");
    restart()
}

/// (Re)starts the running agent, e.g. so a newly granted permission takes effect.
pub fn restart() -> Result<()> {
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", &format!("{}/{LABEL}", domain())])
        .status()
        .context("launchctl kickstart")?;
    anyhow::ensure!(status.success(), "launchctl kickstart failed");
    Ok(())
}

pub fn stop() {
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{}/{LABEL}", domain())])
        .stderr(std::process::Stdio::null())
        .status();
}

pub fn uninstall() -> Result<()> {
    stop();
    if plist_path().exists() {
        std::fs::remove_file(plist_path())?;
    }
    Ok(())
}
