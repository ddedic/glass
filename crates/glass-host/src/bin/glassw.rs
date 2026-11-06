//! Windows: runs the Glass host in the background, the way launchd and systemd do on macOS
//! and Linux. Started at logon (the HKCU Run key) with no console window; starts the host
//! (`glass.exe` beside it) with none either, and starts it again when it exits with an
//! error. One per user session. `glass` asks it to restart or stop the host through the
//! named events below (keep them in sync with src/windows/agent.rs). `glassw app` instead
//! opens the app, for the Start menu shortcut.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("glassw is the Windows background launcher; run `glass` instead");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    // The Start menu shortcut: open the app (`glass app`) without a console window.
    if std::env::args().nth(1).as_deref() == Some("app") {
        agent::open_app();
        return;
    }
    if let Err(e) = agent::run() {
        agent::log(&format!("glassw: {e}"));
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod agent {
    use std::io::Write;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, INFINITE, WaitForMultipleObjects};
    use windows::core::w;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn log_dir() -> PathBuf {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join("AppData\\Local"))
            .join("Glass\\Logs")
    }

    pub fn log(line: &str) {
        let _ = std::fs::create_dir_all(log_dir());
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log_dir().join("agent.log")) {
            let _ = writeln!(f, "{line}");
        }
    }

    /// The host binary: `glass.exe` when installed, `glass-host.exe` in a cargo build.
    fn host() -> std::io::Result<PathBuf> {
        let dir = std::env::current_exe()?.parent().map(PathBuf::from).unwrap_or_default();
        Ok(["glass.exe", "glass-host.exe"].iter().map(|n| dir.join(n)).find(|p| p.exists()).unwrap_or_else(|| dir.join("glass.exe")))
    }

    fn start(host: &PathBuf) -> std::io::Result<Child> {
        let _ = std::fs::create_dir_all(log_dir());
        let out = std::fs::OpenOptions::new().create(true).append(true).open(log_dir().join("agent.log"))?;
        Command::new(host)
            .env("GLASS_SUPERVISED", "1")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    }

    pub fn open_app() {
        match host() {
            Ok(host) => {
                if let Err(e) = Command::new(&host).arg("app").creation_flags(CREATE_NO_WINDOW).spawn() {
                    log(&format!("glassw: can't open the app: {e}"));
                }
            }
            Err(e) => log(&format!("glassw: {e}")),
        }
    }

    pub fn run() -> windows::core::Result<()> {
        // SAFETY: named kernel objects, never closed: they last as long as this process.
        let (restart, stop) = unsafe {
            CreateMutexW(None, false, w!("Local\\GlassAgent"))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                return Ok(()); // already running in this session
            }
            (
                CreateEventW(None, false, false, w!("Local\\GlassAgentRestart"))?,
                CreateEventW(None, false, false, w!("Local\\GlassAgentStop"))?,
            )
        };
        let host = host().map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
        let mut failures = 0u32;
        loop {
            let started = Instant::now();
            let mut child = match start(&host) {
                Ok(c) => c,
                Err(e) => {
                    log(&format!("glassw: can't start {}: {e}", host.display()));
                    std::thread::sleep(Duration::from_secs(10));
                    continue;
                }
            };
            let handles = [HANDLE(child.as_raw_handle()), restart, stop];
            // SAFETY: three live handles.
            let woke = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
            match woke.0 - WAIT_OBJECT_0.0 {
                0 => {
                    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
                    if code == 0 {
                        log("glassw: the host quit; stopping");
                        return Ok(());
                    }
                    // Back off when it keeps failing right away (up to half a minute).
                    failures = if started.elapsed() < Duration::from_secs(30) { failures + 1 } else { 0 };
                    let wait = Duration::from_secs((2u64 << failures.min(4)).min(30));
                    log(&format!("glassw: the host exited ({code}); starting it again in {}s", wait.as_secs()));
                    std::thread::sleep(wait);
                }
                1 => {
                    log("glassw: restarting the host");
                    let _ = child.kill();
                    let _ = child.wait();
                    failures = 0;
                }
                _ => {
                    log("glassw: stopping");
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
            }
        }
    }
}
