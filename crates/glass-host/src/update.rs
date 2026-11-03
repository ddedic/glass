//! Knowing about and installing new releases. The host checks glassdesk.dev/version.json
//! now and then; a client can ask the host to update itself, which re-runs the installer
//! in its own process group so it survives the service restart it triggers.

use std::sync::Mutex;
use std::time::Duration;

const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

static LATEST: Mutex<Option<String>> = Mutex::new(None);

/// The site to update from; GLASS_BASE overrides it (as for the installer).
pub fn base_url() -> String {
    std::env::var("GLASS_BASE").unwrap_or_else(|_| "https://glassdesk.dev".into())
}

/// The newest release we know of, if it's newer than this one and this copy can install it.
pub fn available() -> Option<String> {
    if blocked().is_some() {
        return None;
    }
    LATEST.lock().unwrap().clone().filter(|v| newer(v, CURRENT))
}

/// Why this copy can't update itself, if it can't: a policy turned updates off, or Glass
/// was installed for everyone on the computer (the MSI), which the installer here can't
/// replace: an administrator updates it, with the new MSI or winget.
pub fn blocked() -> Option<&'static str> {
    if !crate::policy::load().allows("updates") {
        return Some("Updates are managed by your organization.");
    }
    #[cfg(windows)]
    if crate::platform::agent::machine_dir().is_some() {
        return Some("Glass is installed for everyone on this computer: an administrator updates it with the new MSI, or winget upgrade Glassdesk.Glass.");
    }
    None
}

/// Starts the periodic version check. `GLASS_TEST_LATEST` pins the result (tests).
pub fn watch() {
    if let Ok(v) = std::env::var("GLASS_TEST_LATEST") {
        *LATEST.lock().unwrap() = Some(v);
        return;
    }
    if crate::testmode::enabled() || blocked().is_some() {
        return;
    }
    tokio::spawn(async {
        loop {
            match fetch_latest().await {
                Some(v) => {
                    let mut latest = LATEST.lock().unwrap();
                    if newer(&v, CURRENT) && latest.as_deref() != Some(v.as_str()) {
                        tracing::info!("update available: {CURRENT} → {v}");
                        crate::push::activity("Glass update ready", &format!("Version {v} is out (you have {CURRENT})."), "update");
                    }
                    *latest = Some(v);
                }
                None => tracing::debug!("version check failed"),
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

pub async fn fetch_latest() -> Option<String> {
    let url = format!("{}/version.json", base_url());
    let out = tokio::process::Command::new("curl")
        .args(["-fsS", "--max-time", "10", &url])
        .output()
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json["version"].as_str().map(str::to_owned)
}

/// The installer: install.sh through sh, or install.ps1 through PowerShell on Windows.
/// On Windows an update runs `glass --glass-firewall-sync` and the script stops if that
/// does not succeed, so this command fails too and does not report the migration done.
fn installer() -> std::process::Command {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg(format!("irm {}/install.ps1 | iex", base_url()));
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-c", &format!("curl -fsSL {}/install.sh | sh", base_url())]);
        cmd
    };
    cmd.env("GLASS_BASE", base_url());
    cmd
}

/// Re-runs the installer in a new process group, detached, so it outlives this process
/// when it restarts the service.
pub fn start_self_update() -> anyhow::Result<()> {
    if let Some(why) = blocked() {
        anyhow::bail!(why);
    }
    let log = crate::platform::log_dir().join("update.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    let mut cmd = installer();
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(std::fs::File::options().append(true).open(&log)?);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
    }
    cmd.spawn()?;
    tracing::info!("self-update started; log: {}", log.display());
    Ok(())
}

/// The installer, waited for, with its output in update.log (the app's Update button).
pub fn run_logged() -> anyhow::Result<()> {
    if let Some(why) = blocked() {
        anyhow::bail!(why);
    }
    let log = crate::platform::log_dir().join("update.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    let mut cmd = installer();
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(std::fs::File::options().append(true).open(&log)?);
    #[cfg(windows)]
    crate::platform::no_window(&mut cmd);
    let status = cmd.status()?;
    anyhow::ensure!(status.success(), "the update didn't finish; see update.log in Glass's logs");
    Ok(())
}

/// `glass update`: the installer, in the foreground.
pub fn run_cli() -> anyhow::Result<()> {
    if let Some(why) = blocked() {
        anyhow::bail!(why);
    }
    let status = installer().status()?;
    anyhow::ensure!(status.success(), "the installer failed");
    Ok(())
}

/// Semver-ish comparison of dotted numeric versions ("0.10.1" > "0.9.3").
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }
}
