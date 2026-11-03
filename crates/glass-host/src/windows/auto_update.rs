//! Silent updates for an MSI install. The service is already LocalSystem, so it can run
//! `msiexec /qn` with no UAC prompt and no window. The package is the official
//! Ntarasiuk/glass GitHub release, checked against the `.sha256` file published beside it.
//!
//! The MSI's own upgrade closes `glass.exe` and Windows starts this service again on the
//! new binary (see the package and `restart_on_failure`). This module does not touch
//! firewall rules; the package being installed does.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::update::{
    HostViewers, LATEST_RELEASE_URL, PlannedDownload, SilentDecision, SilentInput,
};

const START_DELAY: Duration = Duration::from_secs(120);
const DEFER_RETRY: Duration = Duration::from_secs(60);
const MSI_LIMIT: u64 = 64 << 20;

struct Clock {
    next: Instant,
    launched: Option<String>,
}

static CLOCK: Mutex<Option<Clock>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);
static PLAN: Mutex<Option<(Instant, PlannedDownload)>> = Mutex::new(None);

enum Pace {
    Defer,
    Later,
}

/// Called about once a second from the service loop. The check itself runs on another thread.
pub fn tick() {
    if BUSY.load(Ordering::SeqCst) {
        return;
    }
    let mut clock = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    let entry = clock.get_or_insert_with(|| Clock {
        next: now + START_DELAY,
        launched: None,
    });
    if now < entry.next || BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    drop(clock);
    std::thread::spawn(|| {
        struct ClearBusy;
        impl Drop for ClearBusy {
            fn drop(&mut self) {
                BUSY.store(false, Ordering::SeqCst);
            }
        }
        let _clear = ClearBusy;
        let pace = match attempt() {
            Ok(pace) => pace,
            Err(e) => {
                log(&format!("update failed: {e:#}"));
                Pace::Later
            }
        };
        let wait = match pace {
            Pace::Defer => DEFER_RETRY,
            Pace::Later => crate::update::CHECK_EVERY,
        };
        let mut clock = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = clock.as_mut() {
            entry.next = Instant::now() + wait;
        }
        drop(clock);
    });
}

fn attempt() -> Result<Pace> {
    if !machine_install() || !crate::policy::load().allows("updates") {
        return Ok(Pace::Later);
    }
    let dir = ensure_updates_dir()?;
    let plan = cached_plan()?;
    if CLOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.launched.as_deref())
        == Some(plan.version.as_str())
    {
        return Ok(Pace::Later);
    }
    let viewers = viewer_states();
    match crate::update::decide_silent(SilentInput {
        current: crate::update::CURRENT,
        latest: Some(&plan.version),
        updates_allowed: true,
        machine_install: true,
        viewers: &viewers,
    }) {
        SilentDecision::Skip => Ok(Pace::Later),
        SilentDecision::Defer => {
            log(&format!(
                "update {} waiting; a viewer is connected",
                plan.version
            ));
            Ok(Pace::Defer)
        }
        SilentDecision::Install { version } => {
            install(&dir, &plan).with_context(|| format!("install {version}"))?;
            if let Ok(mut clock) = CLOCK.lock()
                && let Some(entry) = clock.as_mut()
            {
                entry.launched = Some(version);
            }
            Ok(Pace::Later)
        }
    }
}

fn machine_install() -> bool {
    let Some(dir) = super::agent::machine_dir() else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    crate::update::path_is_inside(&dir.to_string_lossy(), &exe.to_string_lossy())
}

fn cached_plan() -> Result<PlannedDownload> {
    if let Some((at, plan)) = PLAN.lock().unwrap_or_else(|e| e.into_inner()).clone()
        && at.elapsed() < crate::update::CHECK_EVERY
    {
        return Ok(plan);
    }
    let plan = fetch_plan()?;
    *PLAN.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), plan.clone()));
    Ok(plan)
}

fn fetch_plan() -> Result<PlannedDownload> {
    tls();
    let body = fetch(LATEST_RELEASE_URL, 1 << 20, false)?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        _ => anyhow::bail!("no official Windows MSI for this architecture"),
    };
    crate::update::plan_from_release_json(&String::from_utf8(body)?, arch)
        .context("the latest release has no Windows MSI and checksum")
}

fn install(dir: &Path, plan: &PlannedDownload) -> Result<()> {
    let sha = String::from_utf8(fetch(&plan.sha_url, 4096, true)?)?;
    let bytes = fetch(&plan.msi_url, MSI_LIMIT, true)?;
    anyhow::ensure!(
        crate::update::sha256_file_matches(&bytes, &sha, &plan.msi_name),
        "checksum mismatch"
    );
    let partial = dir.join(format!("{}.partial", plan.msi_name));
    let dest = dir.join(&plan.msi_name);
    let _cleanup = RemoveOnDrop(&partial);
    if reparse(&partial) || reparse(&dest) {
        anyhow::bail!("the update file is a link");
    }
    std::fs::write(&partial, &bytes)?;
    let written = std::fs::read(&partial)?;
    anyhow::ensure!(
        crate::update::sha256_file_matches(&written, &sha, &plan.msi_name),
        "checksum mismatch after write"
    );
    lock_acl(dir)?;
    if dest.exists() {
        anyhow::ensure!(!reparse(&dest), "the update file is a link");
        let _ = std::fs::remove_file(&dest);
    }
    std::fs::rename(&partial, &dest)?;
    let program_data = program_data().to_string_lossy().into_owned();
    anyhow::ensure!(
        crate::update::msi_destination_ok(&program_data, &dest.to_string_lossy(), &plan.msi_name),
        "refusing to run an MSI from {}",
        dest.display()
    );
    // Recheck after the rename. A link planted in this directory is not a package we wrote.
    anyhow::ensure!(
        !reparse(dir) && !reparse(&dest),
        "the update path is a link"
    );
    let code = run_msiexec(&dest)?;
    log(&format!("msiexec {} exit {code:?}", plan.msi_name));
    match code {
        Some(0 | 3010 | 1641) => Ok(()),
        Some(code) => anyhow::bail!("msiexec exited {code}"),
        None => anyhow::bail!("msiexec was still running after 15 minutes"),
    }
}

struct RemoveOnDrop<'a>(&'a Path);
impl Drop for RemoveOnDrop<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.0);
    }
}

/// Downloads `start`, following a redirect only onto GitHub's release CDN.
fn fetch(start: &str, limit: u64, follow_cdn: bool) -> Result<Vec<u8>> {
    if follow_cdn {
        anyhow::ensure!(
            start.starts_with("https://github.com/Ntarasiuk/glass/releases/download/v"),
            "not an official release url"
        );
    } else {
        anyhow::ensure!(
            start == LATEST_RELEASE_URL,
            "not the official release listing"
        );
    }
    tls();
    let agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(180)))
        .user_agent("glass-host")
        .build()
        .new_agent();
    let mut url = start.to_string();
    for hop in 0..4 {
        if hop == 0 {
            anyhow::ensure!(url == start, "url changed");
        } else {
            anyhow::ensure!(
                crate::update::release_cdn_url(&url),
                "redirect left the GitHub release"
            );
        }
        let mut request = agent.get(&url);
        if !follow_cdn {
            request = request.header("accept", "application/vnd.github+json");
        }
        let mut response = request.call().context("download")?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            anyhow::ensure!(follow_cdn, "the release listing redirected");
            let loc = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .context("redirect without a location")?
                .to_string();
            anyhow::ensure!(loc.starts_with("https://"), "redirect is not https");
            url = loc;
            continue;
        }
        anyhow::ensure!(status == 200, "http {status}");
        return Ok(response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()?);
    }
    anyhow::bail!("too many redirects")
}

fn tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn viewer_states() -> Vec<HostViewers> {
    let mut hosts = vec![ask_viewers(&super::service::signin_dir())];
    match console_config_dir() {
        Ok(Some(dir)) => hosts.push(ask_viewers(&dir)),
        Ok(None) => {}
        Err(()) => hosts.push(HostViewers::Unknown),
    }
    hosts
}

fn ask_viewers(dir: &Path) -> HostViewers {
    let dir = dir.to_path_buf();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::ipc::viewer_count(&dir));
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(Some(n))) => HostViewers::Count(n),
        Ok(Ok(None)) => HostViewers::Down,
        _ => HostViewers::Unknown,
    }
}

/// The signed-in user's Glass config, or `Ok(None)` when the console has nobody.
/// `Err` means a person may be there and we could not find their host.
fn console_config_dir() -> std::result::Result<Option<PathBuf>, ()> {
    use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == 0xFFFF_FFFF || session == 0 {
        return Ok(None);
    }
    let Some(sid) = super::secure::console_user_sid() else {
        return Err(());
    };
    let Some(profile) = profile_path(&sid) else {
        return Err(());
    };
    Ok(Some(profile.join(r"AppData\Roaming\Glass")))
}

/// ProfileImagePath is REG_EXPAND_SZ. The shared registry helper only reads REG_SZ, and
/// its buffer is too short for some profile paths; either miss would look like "someone
/// might be connected" and the update would wait forever on a signed-in PC.
fn profile_path(sid: &str) -> Option<PathBuf> {
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::HSTRING;
    let key = HSTRING::from(format!(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\{sid}"
    ));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: buf and len describe a writable buffer. Without RRF_NOEXPAND, an
    // expandable path such as %SystemDrive%\Users\Name is expanded.
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            &HSTRING::from("ProfileImagePath"),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let chars = (len as usize / 2).saturating_sub(1);
    let path = String::from_utf16_lossy(&buf[..chars.min(buf.len())]);
    let path = path.trim_end_matches('\0').trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

fn ensure_updates_dir() -> Result<PathBuf> {
    let glass = program_data().join("Glass");
    anyhow::ensure!(!reparse(&glass), "Glass data directory is a link");
    std::fs::create_dir_all(&glass)?;
    let dir = glass.join("updates");
    if reparse(&dir) {
        std::fs::remove_dir(&dir).context("remove the updates link")?;
    }
    std::fs::create_dir_all(&dir)?;
    anyhow::ensure!(!reparse(&dir), "updates directory is a link");
    lock_acl(&dir)?;
    Ok(dir)
}

fn lock_acl(dir: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(system32().join("icacls.exe"));
    cmd.arg(dir).args([
        "/inheritance:r",
        "/grant:r",
        "*S-1-5-18:(OI)(CI)F",
        "*S-1-5-32-544:(OI)(CI)F",
    ]);
    super::no_window(&mut cmd);
    let status = cmd
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    anyhow::ensure!(status.success(), "could not lock the updates directory");
    let sddl = directory_sddl(dir)?;
    anyhow::ensure!(
        crate::update::sddl_locked_to_system_and_admins(&sddl),
        "updates directory is not limited to SYSTEM and Administrators"
    );
    Ok(())
}

fn directory_sddl(dir: &Path) -> Result<String> {
    let mut cmd =
        std::process::Command::new(system32().join(r"WindowsPowerShell\v1.0\powershell.exe"));
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        "$OutputEncoding = [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false; (Get-Acl -LiteralPath $env:GLASS_UPDATES_DIR).Sddl",
    ]);
    cmd.env("GLASS_UPDATES_DIR", dir);
    super::no_window(&mut cmd);
    let out = cmd.output()?;
    anyhow::ensure!(
        out.status.success(),
        "could not read the updates directory ACL"
    );
    Ok(decode_output(&out.stdout).trim().to_string())
}

fn decode_output(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn run_msiexec(msi: &Path) -> Result<Option<i32>> {
    use std::os::windows::process::CommandExt;
    const DETACHED: u32 = 0x0000_0008;
    const NEW_GROUP: u32 = 0x0000_0200;
    const BREAKAWAY: u32 = 0x0100_0000;
    const NO_WINDOW: u32 = 0x0800_0000;
    let exe = system32().join("msiexec.exe");
    anyhow::ensure!(exe.is_file(), "msiexec is missing");
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("/i").arg(msi).args(["/qn", "/norestart"]);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd.creation_flags(DETACHED | NEW_GROUP | BREAKAWAY | NO_WINDOW);
    let mut child = cmd.spawn().context("start msiexec")?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status.code());
        }
        if started.elapsed() > Duration::from_secs(15 * 60) {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn reparse(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

fn system32() -> PathBuf {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    PathBuf::from(root).join("System32")
}

fn log(msg: &str) {
    let path = program_data().join(r"Glass\updates\update.log");
    // Never create the directory here (it would be writable by users), and never follow
    // a link into somewhere else.
    if path
        .parent()
        .is_none_or(|dir| !dir.is_dir() || reparse(dir))
        || reparse(&path)
    {
        return;
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "{stamp} {msg}");
}
