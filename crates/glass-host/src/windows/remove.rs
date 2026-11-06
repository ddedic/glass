//! Glass in Settings › Apps › Installed apps, for a per-user install (the MSI has its own
//! entry), and what its Uninstall button runs: `glass uninstall --everything`, which takes
//! away all Glass added except its settings.

use std::path::PathBuf;

use anyhow::{Context, Result};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RegDeleteTreeW, RegSetKeyValueW};
use windows::core::HSTRING;

use super::firewall;
use super::ps_string as ps;

const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Glass";

/// The per-user Start menu shortcut.
pub fn start_menu_link() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::home().join("AppData").join("Roaming"))
        .join(r"Microsoft\Windows\Start Menu\Programs\Glass.lnk")
}

/// Where a per-user install lives (install.ps1), and the state next to it.
fn prefix() -> PathBuf {
    super::local_app_data().join("Programs").join("Glass")
}

fn state() -> PathBuf {
    super::local_app_data().join("Glass")
}

/// Adds or refreshes the Installed apps entry (name, version, icon, Uninstall).
pub fn register() -> Result<()> {
    if super::agent::machine_dir().is_some() {
        return Ok(());
    }
    let exe = std::env::current_exe()?;
    let text = |name: &str, value: &str| -> Result<()> {
        let v: Vec<u16> = value.encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated UTF-16 string and its size in bytes; creates the key.
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(KEY), &HSTRING::from(name), REG_SZ.0, Some(v.as_ptr().cast()), (v.len() * 2) as u32)
        }
        .ok()
        .context("add Glass to Installed apps")
    };
    let flag = |name: &str| -> Result<()> {
        let one = 1u32;
        // SAFETY: a DWORD and its size.
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(KEY), &HSTRING::from(name), REG_DWORD.0, Some((&raw const one).cast()), 4) }
            .ok()
            .context("add Glass to Installed apps")
    };
    text("DisplayName", "Glass")?;
    text("DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    text("Publisher", "Glass")?;
    text("URLInfoAbout", "https://glassdesk.dev")?;
    text("DisplayIcon", &state().join("Glass.ico").display().to_string())?;
    text("InstallLocation", &prefix().display().to_string())?;
    text("UninstallString", &format!("\"{}\" uninstall --everything", exe.display()))?;
    flag("NoModify")?;
    flag("NoRepair")?;
    Ok(())
}

/// After `agent::uninstall`: the secure-desktop service and firewall rule (one UAC prompt),
/// the Start menu, Installed apps and PATH entries, then the programs and logs once this
/// process has exited. Settings (%APPDATA%\Glass) stay, in case Glass comes back.
pub fn everything() -> Result<()> {
    let exe = std::env::current_exe()?;
    let service = matches!(super::service::status(), Ok(Some(_)));
    let rule = firewall::named_rule_present(&exe) == Some(true);
    // An MSI install owns the machine-wide scope. Clearing it here would make the next
    // upgrade restore the older any-address rule.
    let clear_scope = super::agent::machine_dir().is_none();
    if service || rule {
        let mut admin = String::from("$ErrorActionPreference = 'Continue'; ");
        if service {
            admin += &format!("& {} service uninstall; ", ps(&exe.display().to_string()));
        }
        if rule {
            admin += &format!(
                "Get-NetFirewallApplicationFilter -Program {} | Get-NetFirewallRule | Where-Object DisplayName -eq 'Glass' | Remove-NetFirewallRule; ",
                ps(&exe.display().to_string())
            );
        }
        if clear_scope {
            // A missing value is success. Any other failure fails this step.
            admin += "try { Remove-ItemProperty -LiteralPath 'HKLM:\\SOFTWARE\\Glass' -Name FirewallScope -ErrorAction Stop } catch { $id = [string]$_.FullyQualifiedErrorId; if ($id -notlike '*PathNotFound*' -and $id -notlike '*ItemNotFound*' -and $id -notlike 'System.Management.Automation.PSArgumentException*') { Write-Error 'Could not remove HKLM\\SOFTWARE\\Glass FirewallScope.'; exit 1 } }; ";
        }
        let done = if super::service::is_elevated() {
            super::powershell(&admin).status().is_ok_and(|s| s.success())
        } else {
            elevated(&admin)
        };
        if !done {
            let which = if clear_scope { "service, firewall rule, and saved firewall scope" } else { "service and firewall rule" };
            eprintln!("The secure-desktop {which} weren't removed (administrator approval was declined?). Remove the service later with: glass service uninstall");
        }
    } else if clear_scope && super::service::is_elevated() {
        if let Err(e) = firewall::clear_machine_scope() {
            eprintln!("{e}");
        }
    } else if clear_scope && firewall::machine_scope().is_some() {
        eprintln!("The saved firewall scope wasn't removed. That needs administrator.");
    }

    let _ = std::fs::remove_file(start_menu_link());
    // SAFETY: plain registry call; a missing key is fine.
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(KEY)) };

    let prefix = prefix();
    if !exe.starts_with(&prefix) {
        // A copy somewhere else (a build, a portable copy): not ours to delete.
        return Ok(());
    }
    let bin = prefix.join("bin");
    let script = format!(
        "$bin = {bin}; $path = ([Environment]::GetEnvironmentVariable('Path', 'User') + '') -split ';' | Where-Object {{ $_ -and $_ -ne $bin }}; \
         [Environment]::SetEnvironmentVariable('Path', ($path -join ';'), 'User'); \
         Start-Process cmd.exe -WindowStyle Hidden -ArgumentList ('/c ping -n 4 127.0.0.1 >nul & rmdir /s /q \"' + {prefix} + '\" & rmdir /s /q \"' + {state} + '\"')",
        bin = ps(&bin.display().to_string()),
        prefix = ps(&prefix.display().to_string()),
        state = ps(&state().display().to_string()),
    );
    let ok = super::powershell(&script).status().is_ok_and(|s| s.success());
    anyhow::ensure!(ok, "couldn't remove Glass's files; delete {} by hand", prefix.display());
    Ok(())
}

/// Runs a PowerShell script as administrator: Windows asks once.
fn elevated(script: &str) -> bool {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let run = format!(
        "$p = Start-Process powershell.exe -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{encoded}' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode"
    );
    super::powershell(&run).status().is_ok_and(|s| s.success())
}
