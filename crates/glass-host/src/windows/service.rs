//! The optional secure-desktop service (`glass service install`). It runs as LocalSystem
//! and does one thing: keep a SYSTEM helper (see `windows::secure`) running in the active
//! console session, re-spawning it whenever that session changes or the helper exits. The
//! helper is what can see and drive the lock screen, UAC prompts and elevated windows; the
//! per-user host stays exactly as it was and remains the only thing that serves and
//! authenticates viewers.
//!
//! While nobody is signed in at the console (after a restart), it also runs the sign-in
//! host there (see `signin.rs`), from the copy of the owner's settings `install` makes, and
//! stops it the moment someone signs in.
//!
//! The service account is LocalSystem so it can duplicate its own token into the console
//! session and call SendSAS; nothing else about Glass moves to SYSTEM. Install also sets
//! the SoftwareSASGeneration policy so SendSAS (Ctrl+Alt+Del) is allowed.

use std::sync::atomic::{AtomicIsize, Ordering};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::core::{HSTRING, PWSTR};

/// The flag the SCM starts us with (see main.rs).
pub const SERVICE_ARG: &str = "--glass-service";
const SERVICE_NAME: &str = "GlassSecureDesktop";
const DISPLAY_NAME: &str = "Glass Secure Desktop";

/// `glass service <install|uninstall|status>`. `remove_host_rule` is the MSI's full uninstall.
/// `firewall_scope` is the word classified before elevation; the elevated process does not
/// read settings.
pub fn cli(action: &str, remove_host_rule: bool, firewall_scope: Option<&str>) -> Result<()> {
    match action {
        "install" => install(firewall_scope),
        "uninstall" => uninstall(remove_host_rule),
        "status" => {
            match status()? {
                Some(true) => println!("The Glass secure-desktop service is installed and running."),
                Some(false) => println!("The Glass secure-desktop service is installed but not running."),
                None => println!(
                    "The Glass secure-desktop service isn't installed. Install it (lets viewers use the\n\
                     lock screen, answer UAC, control elevated windows and send Ctrl+Alt+Del) with:\n  glass service install"
                ),
            }
            Ok(())
        }
        other => anyhow::bail!("unknown action {other}"),
    }
}

/// Whether the service exists, and if so whether it's running. `None` = not installed.
pub fn status() -> Result<Option<bool>> {
    use windows::Win32::System::Services::{
        OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
        SERVICE_STATUS,
    };
    // Query rights only: an ordinary (unelevated) user can't open the service for more, and
    // would read an installed service as missing.
    // SAFETY: handles are closed on every path out.
    unsafe {
        let scm = match OpenSCManagerW(None, None, SC_MANAGER_CONNECT) {
            Ok(h) => h,
            Err(_) => return Ok(None),
        };
        let svc = OpenServiceW(scm, &HSTRING::from(SERVICE_NAME), SERVICE_QUERY_STATUS);
        let out = match svc {
            Ok(svc) => {
                let mut st = SERVICE_STATUS::default();
                let running = QueryServiceStatus(svc, &mut st).is_ok() && st.dwCurrentState == SERVICE_RUNNING;
                let _ = close_service_handle(svc.0);
                Some(running)
            }
            Err(_) => None,
        };
        let _ = close_service_handle(scm.0);
        Ok(out)
    }
}

/// Closes an SC_HANDLE (windows-rs doesn't wrap CloseServiceHandle on SC_HANDLE directly).
unsafe fn close_service_handle(h: *mut core::ffi::c_void) -> bool {
    // SAFETY: closing a service-control handle we opened.
    unsafe { windows::Win32::System::Services::CloseServiceHandle(windows::Win32::System::Services::SC_HANDLE(h)).is_ok() }
}

fn install(firewall_scope: Option<&str>) -> Result<()> {
    if !is_elevated() {
        // Classify here, then pass the word. The elevated process must not read settings:
        // a credential prompt runs as the approving administrator, whose profile is empty.
        return match super::firewall::saved_remote() {
            Ok(remote) => elevate(&["install", "--firewall-scope", super::firewall::remote_arg(remote)]),
            Err(()) => {
                if super::firewall::scope_for_service(None, super::firewall::machine_scope().as_deref()).is_none() {
                    println!("Couldn't read the saved listen address, so no firewall rule was added.");
                }
                elevate(&["install"])
            }
        };
    }
    let exe = protected_copy()?;
    let bin = format!("\"{}\" {SERVICE_ARG}", exe.display());
    // The sign-in host runs this copy. The scope is the word we were given, or the one
    // already stored for the installer. Neither path reads the elevated account's settings.
    match super::firewall::scope_for_service(firewall_scope, super::firewall::machine_scope().as_deref()) {
        Some(remote) if !super::firewall::apply_known(&exe, remote) => {
            println!("Couldn't let the sign-in screen's Glass through Windows Firewall; viewers may not reach it.");
        }
        Some(_) => {}
        None => println!("No firewall scope was given, so the sign-in screen's rule was left unchanged."),
    }
    // The sign-in screen after a restart (signin.rs): this user's token and access rules.
    match copy_signin_settings() {
        Ok(()) => println!("Viewers can sign in to this computer after it restarts."),
        Err(e) => println!("Not on the sign-in screen after a restart ({e:#})."),
    }
    use windows::Win32::System::Services::{
        CreateServiceW, OpenSCManagerW, SC_MANAGER_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_ALL_ACCESS,
        SERVICE_ERROR_NORMAL, SERVICE_WIN32_OWN_PROCESS, StartServiceW,
    };
    set_sas_policy(true)?;
    // SAFETY: all strings are NUL-terminated HSTRINGs; handles are closed below.
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS).context("open the service manager (need administrator)")?;
        let svc = CreateServiceW(
            scm,
            &HSTRING::from(SERVICE_NAME),
            &HSTRING::from(DISPLAY_NAME),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            &HSTRING::from(bin.as_str()),
            None,
            None,
            None,
            None, // lpServiceStartName = null → LocalSystem
            None,
        );
        let svc = match svc {
            Ok(svc) => svc,
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_SERVICE_EXISTS.to_hresult() => {
                // Installed already: point it at the fresh copy and restart it, so
                // `glass service install` also brings it up to date after an update.
                let _ = close_service_handle(scm.0);
                refresh(&bin)?;
                println!("Updated and restarted the Glass secure-desktop service.");
                return Ok(());
            }
            Err(e) => {
                let _ = close_service_handle(scm.0);
                return Err(e).context("create the service");
            }
        };
        restart_on_failure(svc);
        let started = StartServiceW(svc, None).is_ok();
        let _ = close_service_handle(svc.0);
        let _ = close_service_handle(scm.0);
        if started {
            println!("Installed and started the Glass secure-desktop service.");
        } else {
            println!("Installed the Glass secure-desktop service; it starts at the next sign-in or reboot.");
        }
    }
    Ok(())
}

/// The binary the service runs, somewhere only administrators can change: the all-users
/// (MSI) install in Program Files, or else a copy of this binary in Program Files\Glass\Service.
/// A SYSTEM service must never run a file from the user's own folders, or anything running
/// as the user could replace it and get SYSTEM.
fn protected_copy() -> Result<std::path::PathBuf> {
    if let Some(dir) = super::agent::machine_dir() {
        return Ok(dir.join(r"bin\glass.exe"));
    }
    let exe = std::env::current_exe()?;
    let program_files = std::env::var_os("ProgramFiles").context("no ProgramFiles folder")?;
    let dir = std::path::PathBuf::from(program_files).join(r"Glass\Service");
    // Program Files' own permissions are inherited: administrators and SYSTEM may write,
    // users may only read and run.
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let dest = dir.join("glass.exe");
    // The running service holds the old copy open; move it aside first (it's deleted on the
    // next install).
    for old in std::fs::read_dir(&dir)?.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".old")) {
        let _ = std::fs::remove_file(old.path());
    }
    if dest.exists() {
        let _ = std::fs::rename(&dest, dir.join(format!("glass.exe.{}.old", std::process::id())));
    }
    std::fs::copy(&exe, &dest).with_context(|| format!("copy Glass to {}", dest.display()))?;
    // Development builds need their runtime DLLs beside them; release builds have none.
    if let Some(src) = exe.parent() {
        for dll in std::fs::read_dir(src)?.flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".dll")) {
            let _ = std::fs::copy(dll.path(), dir.join(dll.file_name()));
        }
    }
    // The web client, where the copy looks for it (<prefix>\share\glass\web, the prefix
    // being Program Files\Glass): the sign-in host serves it.
    if let Some(web) = exe.parent().and_then(|bin| bin.parent()).map(|p| p.join(r"share\glass\web")).filter(|d| d.is_dir()) {
        let to = dir.parent().context("no Glass folder")?.join(r"share\glass\web");
        let _ = std::fs::remove_dir_all(&to);
        copy_all(&web, &to).with_context(|| format!("copy the web client to {}", to.display()))?;
    }
    Ok(dest)
}

/// Points the installed service at `bin` and restarts it.
fn refresh(bin: &str) -> Result<()> {
    use windows::Win32::System::Services::{
        ChangeServiceConfigW, ControlService, ENUM_SERVICE_TYPE, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
        SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS, SERVICE_CONTROL_STOP, SERVICE_ERROR, SERVICE_NO_CHANGE,
        SERVICE_START_TYPE, SERVICE_STATUS, SERVICE_STOPPED, StartServiceW,
    };
    // SAFETY: handles closed on every path below.
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS).context("open the service manager")?;
        let svc = match OpenServiceW(scm, &HSTRING::from(SERVICE_NAME), SERVICE_ALL_ACCESS) {
            Ok(svc) => svc,
            Err(e) => {
                let _ = close_service_handle(scm.0);
                return Err(e).context("open the service");
            }
        };
        let changed = ChangeServiceConfigW(
            svc,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_START_TYPE(SERVICE_NO_CHANGE),
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            &HSTRING::from(bin),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        restart_on_failure(svc);
        let mut st = SERVICE_STATUS::default();
        let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
        for _ in 0..50 {
            if QueryServiceStatus(svc, &mut st).is_ok() && st.dwCurrentState == SERVICE_STOPPED {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = StartServiceW(svc, None);
        let _ = close_service_handle(svc.0);
        let _ = close_service_handle(scm.0);
        changed.context("update the service")?;
    }
    Ok(())
}

/// Windows starts the service again if it dies: the MSI's installer closes every
/// glass.exe while it copies files (this one included), and a crash shouldn't leave the
/// lock screen unreachable until the next reboot.
///
/// # Safety
/// `svc` is an open service handle with SERVICE_CHANGE_CONFIG.
unsafe fn restart_on_failure(svc: windows::Win32::System::Services::SC_HANDLE) {
    use windows::Win32::System::Services::{
        ChangeServiceConfig2W, SC_ACTION, SC_ACTION_RESTART, SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_FAILURE_ACTIONSW,
    };
    let mut actions = [SC_ACTION { Type: SC_ACTION_RESTART, Delay: 5000 }; 3];
    let config = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 24 * 3600,
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    // SAFETY: the config and its actions live for the call.
    let _ = unsafe { ChangeServiceConfig2W(svc, SERVICE_CONFIG_FAILURE_ACTIONS, Some((&raw const config).cast())) };
}

fn uninstall(remove_host_rule: bool) -> Result<()> {
    if !is_elevated() {
        return elevate(if remove_host_rule { &["uninstall", "--remove-host-rule"] } else { &["uninstall"] });
    }
    // Only a full MSI uninstall asks for this. That action runs this file, so the
    // program is current_exe, including a custom install directory. A manual
    // `glass service uninstall` leaves the desktop host's rule alone.
    if remove_host_rule {
        if let Ok(exe) = std::env::current_exe()
            && !super::firewall::remove_rule_quietly(&exe)
        {
            println!("Couldn't remove the Glass firewall rule for {}.", exe.display());
        }
        if let Err(e) = super::firewall::clear_machine_scope() {
            println!("{e}");
        }
    }
    use windows::Win32::System::Services::{
        ControlService, DeleteService, OpenSCManagerW, OpenServiceW, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
        SERVICE_CONTROL_STOP, SERVICE_STATUS,
    };
    // SAFETY: handles closed on every path; a missing service is reported, not fatal.
    unsafe {
        let scm = OpenSCManagerW(None, None, SC_MANAGER_ALL_ACCESS).context("open the service manager (need administrator)")?;
        let svc = match OpenServiceW(scm, &HSTRING::from(SERVICE_NAME), SERVICE_ALL_ACCESS) {
            Ok(svc) => svc,
            Err(_) => {
                let _ = close_service_handle(scm.0);
                println!("The Glass secure-desktop service isn't installed.");
                return Ok(());
            }
        };
        let mut st = SERVICE_STATUS::default();
        let _ = ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
        let deleted = DeleteService(svc).is_ok();
        let _ = close_service_handle(svc.0);
        let _ = close_service_handle(scm.0);
        anyhow::ensure!(deleted, "couldn't remove the service");
    }
    let _ = set_sas_policy(false);
    let _ = std::fs::remove_dir_all(signin_dir());
    // The per-user service copy's rule. An MSI install's rule is removed above.
    if super::agent::machine_dir().is_none()
        && let Some(pf) = std::env::var_os("ProgramFiles")
    {
        super::firewall::remove_rule_quietly(&std::path::PathBuf::from(&pf).join(r"Glass\Service\glass.exe"));
        let _ = std::fs::remove_dir_all(std::path::PathBuf::from(&pf).join(r"Glass\share"));
    }
    println!("Removed the Glass secure-desktop service.");
    Ok(())
}

/// Turns the SoftwareSASGeneration policy on (so SendSAS works) or off.
fn set_sas_policy(on: bool) -> Result<()> {
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, REG_DWORD, RegDeleteKeyValueW, RegSetKeyValueW,
    };
    const KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
    const VALUE: &str = "SoftwareSASGeneration";
    // SAFETY: plain registry writes under HKLM (we're elevated).
    unsafe {
        if on {
            let one: u32 = 1;
            RegSetKeyValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(KEY),
                &HSTRING::from(VALUE),
                REG_DWORD.0,
                Some((&raw const one).cast()),
                4,
            )
            .ok()
            .context("set the SoftwareSASGeneration policy")?;
        } else {
            let _ = RegDeleteKeyValueW(HKEY_LOCAL_MACHINE, &HSTRING::from(KEY), &HSTRING::from(VALUE));
        }
    }
    Ok(())
}

/// Re-runs `glass service <action>` as administrator (one UAC prompt), like the firewall step.
fn elevate(extra: &[&str]) -> Result<()> {
    let exe = std::env::current_exe()?;
    let list = std::iter::once("service").chain(extra.iter().copied()).map(super::ps_string).collect::<Vec<_>>().join(",");
    let script = format!(
        "$ErrorActionPreference='Stop'; $p = Start-Process -FilePath {} -ArgumentList {list} -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode",
        super::ps_string(&exe.display().to_string())
    );
    let ok = super::powershell(&script).status().is_ok_and(|s| s.success());
    anyhow::ensure!(ok, "administrator approval was declined, or the elevated step failed");
    Ok(())
}

/// Whether this process is elevated (runs with a full administrator token).
pub(super) fn is_elevated() -> bool {
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    // SAFETY: the token is closed right after the query.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elevation.TokenIsElevated != 0
    }
}

// ---- Running as the service ----

/// Set by the control handler, read by the keep-alive loop. HANDLEs stored as isize so the
/// `extern "system"` handler can reach them.
static STOP_EVENT: AtomicIsize = AtomicIsize::new(0);
static WAKE_EVENT: AtomicIsize = AtomicIsize::new(0);
static STATUS_HANDLE: AtomicIsize = AtomicIsize::new(0);

/// The SCM entry point (`glass --glass-service`).
pub fn run_service() -> Result<()> {
    use windows::Win32::System::Services::{SERVICE_TABLE_ENTRYW, StartServiceCtrlDispatcherW};
    let mut name: Vec<u16> = SERVICE_NAME.encode_utf16().chain([0]).collect();
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: a null-terminated table whose names live for the call (the dispatcher blocks
    // here until the service stops).
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.context("StartServiceCtrlDispatcher")?;
    Ok(())
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    use windows::Win32::System::Services::RegisterServiceCtrlHandlerExW;
    // SAFETY: registering our handler; the returned status handle is stored for updates.
    let handle = unsafe { RegisterServiceCtrlHandlerExW(&HSTRING::from(SERVICE_NAME), Some(handler), None) };
    let Ok(handle) = handle else { return };
    STATUS_HANDLE.store(handle.0 as isize, Ordering::SeqCst);

    // SAFETY: three auto-reset events, kept for the service's life. Only `sas` is
    // inheritable: the helper gets it to ask for Ctrl+Alt+Del.
    let (stop, wake, sas) = unsafe {
        use windows::Win32::Security::SECURITY_ATTRIBUTES;
        use windows::Win32::System::Threading::CreateEventW;
        let inherit = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        (
            CreateEventW(None, false, false, None).unwrap_or_default(),
            CreateEventW(None, false, false, None).unwrap_or_default(),
            CreateEventW(Some(&inherit), false, false, None).unwrap_or_default(),
        )
    };
    STOP_EVENT.store(stop.0 as isize, Ordering::SeqCst);
    WAKE_EVENT.store(wake.0 as isize, Ordering::SeqCst);

    set_status(SERVICE_STATE_RUNNING, 0);
    keep_helper_alive(stop, wake, sas);
    set_status(SERVICE_STATE_STOPPED, 0);
}

const SERVICE_STATE_RUNNING: u32 = 4;
const SERVICE_STATE_STOPPED: u32 = 1;
const SERVICE_STATE_STOP_PENDING: u32 = 3;

/// Tells the SCM what state we're in, accepting stop, shutdown and session-change.
fn set_status(state: u32, checkpoint: u32) {
    use windows::Win32::System::Services::{
        SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_STATUS,
        SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    };
    let handle = STATUS_HANDLE.load(Ordering::SeqCst);
    if handle == 0 {
        return;
    }
    let accepts = if state == SERVICE_STATE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: SERVICE_STATUS_CURRENT_STATE(state),
        dwControlsAccepted: accepts,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: checkpoint,
        dwWaitHint: if state == SERVICE_STATE_STOP_PENDING { 3000 } else { 0 },
    };
    // SAFETY: a valid status handle and a fully-initialized status.
    unsafe { let _ = SetServiceStatus(SERVICE_STATUS_HANDLE(handle as *mut _), &status); }
}

unsafe extern "system" fn handler(control: u32, _event: u32, _data: *mut core::ffi::c_void, _ctx: *mut core::ffi::c_void) -> u32 {
    use windows::Win32::System::Services::{SERVICE_CONTROL_SESSIONCHANGE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP};
    use windows::Win32::System::Threading::SetEvent;
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            set_status(SERVICE_STATE_STOP_PENDING, 1);
            let stop = STOP_EVENT.load(Ordering::SeqCst);
            // SAFETY: signalling the stop event if it exists.
            if stop != 0 { unsafe { let _ = SetEvent(HANDLE(stop as *mut _)); } }
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            let wake = WAKE_EVENT.load(Ordering::SeqCst);
            // SAFETY: nudging the loop to re-check the active session.
            if wake != 0 { unsafe { let _ = SetEvent(HANDLE(wake as *mut _)); } }
        }
        _ => {}
    }
    0
}

/// Keeps the SYSTEM helper running in the active console session until the service stops.
fn keep_helper_alive(stop: HANDLE, wake: HANDLE, sas: HANDLE) {
    use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
    use windows::Win32::System::Threading::WaitForMultipleObjects;
    let mut child: Option<HANDLE> = None;
    let mut child_session = 0xFFFF_FFFFu32;
    // The sign-in host (signin.rs), while nobody is signed in at the console.
    let mut signin: Option<HANDLE> = None;
    let mut signin_session = 0xFFFF_FFFFu32;
    loop {
        // SAFETY: plain query.
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        // Reap a helper that exited.
        if let Some(h) = child
            && !super::process_alive_handle(h)
        {
            // SAFETY: closing the exited child's handle.
            unsafe { let _ = CloseHandle(h); }
            child = None;
        }
        // A real interactive session (not the "no one connected" 0xFFFFFFFF, nor session 0).
        let usable = session != 0xFFFF_FFFF && session != 0;
        if usable && (child.is_none() || session != child_session) {
            if let Some(h) = child.take() {
                // Session changed: the old helper is in the wrong session; drop it.
                terminate(h);
            }
            match spawn_in_session(session, super::secure::HELPER_ARG, &[], Some(sas)) {
                Ok(h) => {
                    child = Some(h);
                    child_session = session;
                }
                Err(()) => {}
            }
        }
        // The sign-in host: up while the console session has nobody signed in (and the owner's
        // settings were copied at install), gone the moment someone signs in, so their own
        // host can start in its place.
        if let Some(h) = signin
            && !super::process_alive_handle(h)
        {
            // SAFETY: closing the exited child's handle.
            unsafe { let _ = CloseHandle(h); }
            signin = None;
        }
        let wanted = usable && signin_dir().join("token").exists() && !signed_in(session);
        if wanted && (signin.is_none() || session != signin_session) {
            if let Some(h) = signin.take() {
                terminate(h);
            }
            let env = [("GLASS_CONFIG_DIR", signin_dir().display().to_string()), ("GLASS_NO_MENUBAR", "1".into())];
            if let Ok(h) = spawn_in_session(session, crate::signin::ARG, &env, None) {
                signin = Some(h);
                signin_session = session;
            }
        } else if !wanted && let Some(h) = signin.take() {
            terminate(h);
        }
        // Wake on stop, on a Ctrl+Alt+Del request, on the helper exiting, or every second
        // to re-check the session.
        let mut handles = vec![stop, wake, sas];
        if let Some(h) = child {
            handles.push(h);
        }
        // SAFETY: all handles are live for the wait.
        let woke = unsafe { WaitForMultipleObjects(&handles, false, 1000) };
        if woke == WAIT_OBJECT_0 {
            break; // stop (handles[0])
        }
        if woke.0 == WAIT_OBJECT_0.0 + 2 {
            // Needs the SoftwareSASGeneration policy (install sets it) and a service caller.
            // SAFETY: no preconditions beyond those.
            unsafe { windows::Win32::Security::Authentication::Identity::SendSAS(false) };
        }
    }
    if let Some(h) = child {
        terminate(h);
    }
    if let Some(h) = signin {
        terminate(h);
    }
}

/// Whether someone is signed in to `session` (locked counts: their own host serves that).
fn signed_in(session: u32) -> bool {
    use windows::Win32::System::RemoteDesktop::{WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName};
    // SAFETY: the buffer comes from WTS and is freed with WTSFreeMemory.
    unsafe {
        let mut name = PWSTR::null();
        let mut bytes = 0u32;
        if WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, WTSUserName, &mut name, &mut bytes).is_err() {
            return true; // can't tell: leave it to the user's host
        }
        let someone = !name.is_null() && !name.to_string().unwrap_or_default().is_empty();
        WTSFreeMemory(name.0.cast());
        someone
    }
}

/// Where the sign-in host keeps its copy of the owner's settings: SYSTEM and
/// administrators only.
pub fn signin_dir() -> std::path::PathBuf {
    let data = std::env::var_os("ProgramData").map(std::path::PathBuf::from).unwrap_or_else(|| r"C:\ProgramData".into());
    data.join(r"Glass\signin")
}

/// Copies what the sign-in host needs of this user's settings (signin::FILES) to
/// `signin_dir`, locked to SYSTEM and administrators. Run as administrator (install).
fn copy_signin_settings() -> Result<()> {
    let from = crate::config_dir();
    anyhow::ensure!(from.join("token").exists(), "no token in {} yet: run glass setup first", from.display());
    let to = signin_dir();
    let _ = std::fs::remove_dir_all(&to);
    std::fs::create_dir_all(&to).with_context(|| format!("create {}", to.display()))?;
    // Only SYSTEM and administrators, before anything secret goes in (ProgramData lets
    // every user read by default).
    let mut icacls = std::process::Command::new("icacls");
    icacls.arg(&to).args(["/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F"]);
    super::no_window(&mut icacls);
    let locked = icacls.stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
    anyhow::ensure!(locked, "couldn't restrict {}", to.display());
    for name in crate::signin::FILES {
        copy_all(&from.join(name), &to.join(name))?;
    }
    Ok(())
}

fn copy_all(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)?.flatten() {
            copy_all(&e.path(), &to.join(e.file_name()))?;
        }
    } else if from.is_file() {
        std::fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    }
    Ok(())
}

fn terminate(child: HANDLE) {
    use windows::Win32::System::Threading::TerminateProcess;
    // SAFETY: the child is ours; terminate and close it.
    unsafe {
        let _ = TerminateProcess(child, 0);
        let _ = CloseHandle(child);
    }
}

/// Launches the SYSTEM helper in `session` on the default desktop, by duplicating this
/// service's own (LocalSystem) token and moving the copy into that session. Returns the
/// new process's handle.
fn spawn_in_session(session: u32, arg: &str, env_extra: &[(&str, String)], sas: Option<HANDLE>) -> Result<HANDLE, ()> {
    use windows::Win32::Security::{
        DuplicateTokenEx, SECURITY_IMPERSONATION_LEVEL, SetTokenInformation, TOKEN_ALL_ACCESS, TOKEN_TYPE,
        TokenPrimary, TokenSessionId,
    };
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::Threading::{
        CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    let Ok(exe) = std::env::current_exe() else { return Err(()) };
    // SAFETY: every handle opened here is closed before returning; buffers are sized for
    // the APIs. On any failure we clean up and report Err so the loop retries later.
    unsafe {
        let mut token = HANDLE::default();
        let access = windows::Win32::Security::TOKEN_ACCESS_MASK(
            0x0002 | 0x0008 | 0x0001 | 0x0080 | 0x0100, // DUPLICATE|QUERY|ASSIGN_PRIMARY|ADJUST_DEFAULT|ADJUST_SESSIONID
        );
        if OpenProcessToken(GetCurrentProcess(), access, &mut token).is_err() {
            return Err(());
        }
        let mut dup = HANDLE::default();
        let dup_ok = DuplicateTokenEx(
            token,
            TOKEN_ALL_ACCESS,
            None,
            SECURITY_IMPERSONATION_LEVEL(2), // SecurityImpersonation
            TOKEN_TYPE(TokenPrimary.0),
            &mut dup,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !dup_ok {
            return Err(());
        }
        let sid = session;
        let set = SetTokenInformation(dup, TokenSessionId, (&raw const sid).cast(), 4).is_ok();
        if !set {
            let _ = CloseHandle(dup);
            return Err(());
        }
        let mut env = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env, Some(dup), false).is_ok();
        // SYSTEM's environment, plus `env_extra`: NAME=value strings, each NUL-ended, and
        // one more NUL at the end.
        let mut block: Vec<u16> = Vec::new();
        if have_env {
            let mut p = env as *const u16;
            while *p != 0 {
                let start = p;
                while *p != 0 {
                    p = p.add(1);
                }
                block.extend_from_slice(std::slice::from_raw_parts(start, p.offset_from(start) as usize + 1));
                p = p.add(1);
            }
        }
        for (k, v) in env_extra {
            block.extend(format!("{k}={v}").encode_utf16().chain([0]));
        }
        block.push(0);

        // The helper gets the Ctrl+Alt+Del event's handle value, inherited.
        let line = match sas {
            Some(sas) => format!("\"{}\" {arg} {}", exe.display(), sas.0 as isize),
            None => format!("\"{}\" {arg}", exe.display()),
        };
        let mut cmd: Vec<u16> = line
            .encode_utf16()
            .chain([0])
            .collect();
        let mut desktop: Vec<u16> = "winsta0\\default".encode_utf16().chain([0]).collect();
        let mut si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let flags = CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT;
        let ok = CreateProcessAsUserW(
            Some(dup),
            None,
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            sas.is_some(), // only the inheritable `sas` event goes along
            flags,
            if have_env || !env_extra.is_empty() { Some(block.as_ptr().cast()) } else { None },
            None,
            &mut si,
            &mut pi,
        )
        .is_ok();
        if have_env {
            let _ = DestroyEnvironmentBlock(env);
        }
        let _ = CloseHandle(dup);
        if !ok {
            return Err(());
        }
        if !pi.hThread.is_invalid() {
            let _ = CloseHandle(pi.hThread);
        }
        Ok(pi.hProcess)
    }
}
