//! Windows 10/11: the host runs in the signed-in user's session (a logon task, not a
//! service: services live in session 0 and can't see the desktop).

pub mod agent;
pub mod audio;
pub mod clipboard;
pub mod clock;
pub mod cursor;
pub mod convert;
pub mod firewall;
pub mod front;
pub mod input;
pub mod keymap;
pub mod micdriver;
pub mod permissions;
pub mod pipe;
pub mod policy;
pub mod privacy;
pub mod remove;
pub mod script;
pub mod secure;
pub mod service;
pub mod session;
pub mod setup;
pub mod system;
pub mod toast;
pub mod tray;
pub mod uia;
pub mod video;

use std::process::{Command, Stdio};

use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::core::HSTRING;

pub const OS: &str = "windows";

/// A string value under HKLM.
fn registry_string(key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: buf and len describe a writable buffer of len bytes.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let chars = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
}

/// "Windows 11 Pro 24H2". ProductName still says Windows 10 on 11, so the build decides.
pub fn os_version() -> String {
    const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let build: u32 = registry_string(KEY, "CurrentBuildNumber")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    let product = registry_string(KEY, "ProductName").unwrap_or_else(|| "Windows".into());
    let product = if build >= 22000 { product.replace("Windows 10", "Windows 11") } else { product };
    match registry_string(KEY, "DisplayVersion") {
        Some(v) => format!("{product} {v}"),
        None => product,
    }
}

/// %LOCALAPPDATA%\Glass\Logs.
pub fn log_dir() -> std::path::PathBuf {
    local_app_data().join("Glass").join("Logs")
}

pub fn local_app_data() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::home().join("AppData").join("Local"))
}

/// Keeps the system and displays awake while the host runs. The request belongs to the
/// calling thread (the host's main thread), so it lasts until the process exits.
pub fn keep_awake() -> Option<std::process::Child> {
    // SAFETY: no preconditions.
    if unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) }.0 == 0 {
        tracing::warn!("SetThreadExecutionState failed; the computer may sleep");
    }
    None
}

pub fn has_command(name: &str) -> bool {
    Command::new("where")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs a Windows PowerShell script with no profile and no window.
pub fn powershell(script: &str) -> Command {
    let mut cmd = Command::new("powershell.exe");
    cmd.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script]);
    no_window(&mut cmd);
    cmd
}

/// Children started from the background host shouldn't flash a console window.
pub fn no_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Same flag for the async `tailscale status` used when the owner is still unknown.
pub fn no_window_tokio(cmd: &mut tokio::process::Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// A PowerShell single-quoted string literal.
pub fn ps_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}


/// Whether a process we hold a handle to is still running (zero-timeout wait).
pub fn process_alive_handle(h: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::Foundation::WAIT_TIMEOUT;
    use windows::Win32::System::Threading::WaitForSingleObject;
    // SAFETY: a process handle we own; a zero wait only polls.
    unsafe { WaitForSingleObject(h, 0) == WAIT_TIMEOUT }
}

/// Whether process `pid` is still running.
pub fn process_alive(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: the handle is closed right after the zero-timeout wait.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return false };
        let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
        let _ = CloseHandle(h);
        running
    }
}

/// Desktop coordinates in physical pixels, matching what duplication captures and what
/// SendInput expects, on every display whatever its scaling.
pub fn dpi_aware() {
    use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
    // SAFETY: no preconditions; fails harmlessly if already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}
