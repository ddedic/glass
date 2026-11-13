//! The secure desktop: letting a viewer use the computer when it's locked, answer UAC
//! prompts, control elevated windows and send Ctrl+Alt+Del.
//!
//! The per-user host (running as the signed-in user) can't do these: DXGI duplication
//! can't see the Winlogon/secure desktop and SendInput can't reach it or elevated windows
//! (UIPI). So an optional LocalSystem service (see `windows::service`) keeps a SYSTEM
//! **helper** running in the active console session. The helper captures and injects on
//! whatever the input desktop is (OpenInputDesktop + SetThreadDesktop, so Winlogon/UAC
//! desktops work), and runs SendSAS for Ctrl+Alt+Del. The two talk over a local named pipe
//! with a strict ACL (SYSTEM, Administrators, and the console user only; remote clients
//! rejected); there is no new network listener and the host stays the only thing that
//! authenticates viewers.
//!
//! The host routes to the helper only while it's needed — the secure desktop is up, or the
//! foreground window is elevated — and keeps working unchanged when the service isn't
//! installed. The wire format is deliberately tiny and every message is a fixed size the
//! helper validates, so nothing it reads can grow unbounded.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use glass_proto::InputEvent as Ev;
use tokio::sync::broadcast;

use super::pipe::Pipe;
use crate::media::{EncodedFrame, VideoSource};

/// The flag that starts the SYSTEM helper (spawned by the service in the console session).
pub const HELPER_ARG: &str = "--glass-secure-helper";

/// One helper per machine, serving whoever is at the active console session.
const PIPE: &str = r"\\.\pipe\glass-secure";

// Host -> helper opcodes, each followed by a fixed-size payload the helper reads exactly.
const OP_INPUT: u8 = b'I'; // 12 bytes: a glass_proto::InputEvent (not MouseMove)
const OP_MOVE_ABS: u8 = b'M'; // 8 bytes: i32 x, i32 y (desktop pixels)
const OP_SAS: u8 = b'S'; // 0 bytes: SendSAS (Ctrl+Alt+Del)
const OP_RELEASE: u8 = b'R'; // 0 bytes: release everything held
const OP_CAPTURE: u8 = b'V'; // 1 byte: 1 start sending frames, 0 stop
const OP_KEYFRAME: u8 = b'K'; // 1 byte: stream (0xFF = all)
const OP_BITRATE: u8 = b'B'; // 4 bytes: u32 LE bits per second

// Helper -> host opcodes.
const OP_FRAME: u8 = b'F'; // header + H.264 Annex B: an encoded frame to rebroadcast

// ---- Host side ----

/// The open pipe to the helper; writes from different threads take turns.
struct Link {
    pipe: Arc<Pipe>,
    writing: Mutex<()>,
}

static LINK: Mutex<Option<Arc<Link>>> = Mutex::new(None);
/// The host is currently sourcing viewers' video from the helper (secure desktop up).
static CAPTURING: AtomicBool = AtomicBool::new(false);

/// Starts keeping a connection to the helper (if the service is installed) and driving it:
/// while the secure desktop is up, the helper becomes the video source and its frames are
/// rebroadcast to viewers. A no-op worth nothing when the service isn't there.
pub fn start(frames: broadcast::Sender<Arc<EncodedFrame>>) {
    std::thread::Builder::new()
        .name("glass-secure".into())
        .spawn(move || driver(frames))
        .ok();
}

/// Keeps the link alive and switches the video source as the desktop locks and unlocks.
fn driver(frames: broadcast::Sender<Arc<EncodedFrame>>) {
    loop {
        if LINK.lock().unwrap().is_none() {
            if connect(frames.clone()) {
                tracing::info!("secure desktop: connected to the SYSTEM helper");
            } else {
                // Not installed, or not up yet: try again, cheaply.
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            }
        }
        // The secure desktop (lock screen, UAC) is up when the input desktop isn't ours.
        let secure = !super::session::input_desktop_is_ours();
        if secure != CAPTURING.load(Ordering::Relaxed) {
            if secure {
                tracing::info!("secure desktop up: viewers now see the SYSTEM helper's capture");
                send(OP_CAPTURE, &[1]);
            } else {
                tracing::info!("secure desktop gone: viewers back on the user host's capture");
                send(OP_CAPTURE, &[0]);
            }
            CAPTURING.store(secure, Ordering::Relaxed);
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

/// Opens the helper's pipe and starts reading its frames. False if it isn't there.
fn connect(frames: broadcast::Sender<Arc<EncodedFrame>>) -> bool {
    let pipe = match Pipe::open(PIPE) {
        Ok(p) => Arc::new(p),
        Err(_) => return false,
    };
    // Only SYSTEM's pipe is the helper: another process that took the name first could
    // otherwise show viewers a made-up lock screen.
    if !owned_by_system(pipe.handle()) {
        warn_once("the secure-desktop pipe isn't SYSTEM's; ignoring it");
        return false;
    }
    let mut hello = [0u8; HANDSHAKE_LEN];
    if (&*pipe).read_exact(&mut hello).is_err() {
        return false;
    }
    if hello != handshake() {
        warn_once("the secure-desktop service is from another version of Glass; run `glass service install` to update it");
        return false;
    }
    *LINK.lock().unwrap() = Some(Arc::new(Link { pipe: pipe.clone(), writing: Mutex::new(()) }));
    std::thread::Builder::new()
        .name("glass-secure-rx".into())
        .spawn(move || read_frames(pipe, frames))
        .ok();
    true
}

/// Reads encoded frames from the helper and rebroadcasts them to viewers while the secure
/// desktop is up. Ends (dropping the link) when the pipe closes.
fn read_frames(pipe: Arc<Pipe>, frames: broadcast::Sender<Arc<EncodedFrame>>) {
    let mut pipe = &*pipe;
    loop {
        let mut op = [0u8; 1];
        if pipe.read_exact(&mut op).is_err() {
            break;
        }
        if op[0] != OP_FRAME {
            break; // unexpected: the helper only ever sends frames
        }
        let mut header = [0u8; 14];
        if pipe.read_exact(&mut header).is_err() {
            break;
        }
        let stream = header[0];
        let flags = header[1];
        let capture_us = u64::from_le_bytes(header[2..10].try_into().unwrap());
        let len = u32::from_le_bytes(header[10..14].try_into().unwrap()) as usize;
        if stream as usize >= glass_proto::MAX_STREAMS || len > (16 << 20) {
            break; // out of range: a corrupt or hostile stream, drop the link
        }
        let mut data = vec![0u8; len];
        if pipe.read_exact(&mut data).is_err() {
            break;
        }
        if CAPTURING.load(Ordering::Relaxed) {
            let _ = frames.send(Arc::new(EncodedFrame {
                stream,
                data,
                keyframe: flags & 1 != 0,
                capture_us,
                hevc: false,
                refresh: false,
            }));
        }
    }
    *LINK.lock().unwrap() = None;
    CAPTURING.store(false, Ordering::Relaxed);
}

/// Whether the helper is connected.
pub fn available() -> bool {
    LINK.lock().unwrap().is_some()
}

/// The host is sourcing video from the helper right now (so its own capture is paused).
pub fn capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Whether input should go through the helper now: the secure desktop is up, or the
/// foreground window is elevated (UIPI blocks the host either way). Only when connected.
pub fn route_input() -> bool {
    available() && (!super::session::input_desktop_is_ours() || super::front::foreground_is_elevated())
}

/// Sends one opcode and its fixed payload; drops the link on any write error.
fn send(op: u8, payload: &[u8]) -> bool {
    let link = LINK.lock().unwrap().clone();
    let Some(link) = link else { return false };
    let ok = {
        let _turn = link.writing.lock().unwrap();
        let mut w = &*link.pipe;
        w.write_all(&[op]).and_then(|_| w.write_all(payload)).is_ok()
    };
    if !ok {
        *LINK.lock().unwrap() = None;
    }
    ok
}

/// Forwards an input event to the helper. Absolute moves are mapped to desktop pixels by
/// the caller (`move_abs`); everything else goes as its 12-byte wire form.
pub fn send_input(ev: &Ev) -> bool {
    send(OP_INPUT, &ev.write())
}

/// Text uses the same input opcodes as keys. Write every chunk under one pipe lock so
/// another command cannot land between a piece's chunks.
pub fn type_text(text: &str) -> bool {
    let packet = text_packet(text);
    match packet.split_first() {
        Some((&op, payload)) => send(op, payload),
        None => true,
    }
}

fn text_packet(text: &str) -> Vec<u8> {
    let mut packet = Vec::new();
    for ev in glass_proto::text_events(text) {
        packet.push(OP_INPUT);
        packet.extend_from_slice(&ev.write());
    }
    packet
}

pub fn move_abs(x: i32, y: i32) -> bool {
    let mut p = [0u8; 8];
    p[0..4].copy_from_slice(&x.to_le_bytes());
    p[4..8].copy_from_slice(&y.to_le_bytes());
    send(OP_MOVE_ABS, &p)
}

pub fn release() -> bool {
    send(OP_RELEASE, &[])
}

/// Ctrl+Alt+Del. False when the service isn't installed (the caller tells the viewer).
pub fn send_sas() -> bool {
    send(OP_SAS, &[])
}

/// Forwards viewers' keyframe/bitrate wishes while the helper is the source.
pub fn request_keyframe(stream: Option<u8>) {
    if capturing() {
        send(OP_KEYFRAME, &[stream.unwrap_or(0xFF)]);
    }
}

pub fn set_bitrate(bps: u32) {
    if capturing() {
        send(OP_BITRATE, &bps.to_le_bytes());
    }
}

// ---- Helper side (runs as LocalSystem in the active console session) ----

/// The whole life of the SYSTEM helper: create the strict-ACL pipe, then serve the per-user
/// host — inject on the input desktop, run SendSAS, and stream the secure desktop back.
pub fn run_helper() -> anyhow::Result<()> {
    use windows::Win32::System::Pipes::DisconnectNamedPipe;

    super::dpi_aware();
    IS_HELPER.store(true, Ordering::SeqCst);
    log_helper("secure helper: starting");
    std::thread::Builder::new().name("glass-step-server".into()).spawn(|| {
        if let Err(e) = run_step_server() { log_helper(&format!("native execution transport stopped: {e}")); }
    })?;
    let sd = security_descriptor();
    loop {
        let pipe = Arc::new(create_pipe(&sd, PIPE)?);
        if !pipe.connect() {
            continue;
        }
        match client_check(pipe.handle()) {
            Ok(()) => serve(pipe.clone()),
            Err(why) => log_helper(&format!("secure helper: refused a connection: {why}")),
        }
        // SAFETY: our pipe; disconnect before accepting the next host (dropping closes it).
        let _ = unsafe { DisconnectNamedPipe(pipe.handle()) };
    }
}

/// Serves one connected host until the pipe closes: commands are read here while frames
/// are written from the forwarding thread, at the same time (the pipe is overlapped).
fn serve(pipe: Arc<Pipe>) {
    let mut reader = &*pipe;
    // Which Glass this is: the host refuses a helper from another version.
    if (&*pipe).write_all(&handshake()).is_err() {
        return;
    }

    // The host's pipe for this connection: capture frames are forwarded here while enabled.
    let writer = Arc::new(Mutex::new(pipe.clone()));

    loop {
        let mut op = [0u8; 1];
        if reader.read_exact(&mut op).is_err() {
            break;
        }
        match op[0] {
            OP_SAS => {
                send_sas_now();
            }
            OP_CAPTURE => {
                let mut b = [0u8; 1];
                if reader.read_exact(&mut b).is_err() {
                    break;
                }
                if b[0] != 0 {
                    capture_enable(&writer);
                } else {
                    capture_disable();
                }
            }
            OP_KEYFRAME => {
                let mut b = [0u8; 1];
                if reader.read_exact(&mut b).is_err() {
                    break;
                }
                if let Some(c) = CAPTURE.get() {
                    c.video.request_keyframe((b[0] != 0xFF).then_some(b[0]));
                }
            }
            OP_BITRATE => {
                let mut b = [0u8; 4];
                if reader.read_exact(&mut b).is_err() {
                    break;
                }
                if let Some(c) = CAPTURE.get() {
                    c.video.set_bitrate(u32::from_le_bytes(b));
                }
            }
            _ => break, // nothing else is valid: drop a confused or hostile host
        }
    }
    capture_detach(); // this host is gone; stop forwarding to its (now dead) pipe
    log_helper("secure helper: host disconnected");
}

/// The helper's capture: created once (lazily, on the first lock) and kept for the helper's
/// life — `video::Video` has no shutdown, and one duplication is cheaper than making a new
/// one each time the screen locks. Forwarding is gated by `forward` and the current host's
/// pipe, so nothing is captured or sent while unlocked or between connections.
struct CaptureState {
    video: Arc<super::video::Video>,
    forward: AtomicBool,
    writer: Mutex<Option<Arc<Mutex<Arc<Pipe>>>>>,
}

static CAPTURE: OnceLock<CaptureState> = OnceLock::new();

/// Starts forwarding the secure desktop to `writer`; makes the capture the first time.
fn capture_enable(writer: &Arc<Mutex<Arc<Pipe>>>) {
    let state = CAPTURE.get_or_init(|| {
        let (frames, rx) = broadcast::channel::<Arc<EncodedFrame>>(64);
        let video = Arc::new(super::video::Video::for_helper(
            super::video::VideoConfig { fps: 60, bitrate: 20_000_000, scale: 1.0 },
            frames,
        ));
        if let Err(e) = video.set_layout(crate::media::Layout::Single(None)) {
            log_helper(&format!("secure helper: can't capture the secure desktop ({e:#})"));
        }
        spawn_forward(rx);
        CaptureState { video, forward: AtomicBool::new(false), writer: Mutex::new(None) }
    });
    *state.writer.lock().unwrap() = Some(writer.clone());
    state.forward.store(true, Ordering::Relaxed);
    state.video.request_keyframe(None); // a fresh keyframe so the viewer can switch codec
}

fn capture_disable() {
    if let Some(c) = CAPTURE.get() {
        c.forward.store(false, Ordering::Relaxed);
    }
}

/// A host disconnected: stop forwarding and forget its pipe.
fn capture_detach() {
    if let Some(c) = CAPTURE.get() {
        c.forward.store(false, Ordering::Relaxed);
        *c.writer.lock().unwrap() = None;
    }
}

/// The single, long-lived thread that pushes encoded frames to the current host while
/// forwarding is on.
fn spawn_forward(mut rx: broadcast::Receiver<Arc<EncodedFrame>>) {
    std::thread::Builder::new()
        .name("glass-secure-tx".into())
        .spawn(move || loop {
            let frame = match rx.blocking_recv() {
                Ok(f) => f,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            let Some(state) = CAPTURE.get() else { continue };
            if !state.forward.load(Ordering::Relaxed) {
                continue;
            }
            let writer = state.writer.lock().unwrap().clone();
            if let Some(writer) = writer {
                let _ = write_frame(&writer, &frame);
            }
        })
        .ok();
}

/// Writes one encoded frame to the host (header + Annex B). False on a broken pipe.
fn write_frame(writer: &Mutex<Arc<Pipe>>, frame: &EncodedFrame) -> bool {
    let mut header = [0u8; 15];
    header[0] = OP_FRAME;
    header[1] = frame.stream;
    header[2] = frame.keyframe as u8;
    header[3..11].copy_from_slice(&frame.capture_us.to_le_bytes());
    header[11..15].copy_from_slice(&(frame.data.len() as u32).to_le_bytes());
    let pipe = writer.lock().unwrap();
    let mut w = &**pipe;
    w.write_all(&header).and_then(|_| w.write_all(&frame.data)).is_ok()
}

/// Set once this process is the SYSTEM helper, so shared capture code (video.rs) knows to
/// put its capture threads on the input desktop — needed to duplicate the secure desktop.
static IS_HELPER: AtomicBool = AtomicBool::new(false);

/// This process captures and injects on the input desktop itself: the helper, or the
/// sign-in host (signin.rs), both SYSTEM.
pub fn mark_helper() {
    IS_HELPER.store(true, Ordering::SeqCst);
}

/// Called by each capture thread (video.rs): in the helper, follow the input desktop so
/// DXGI duplication can see the lock screen / UAC desktop; a no-op in the per-user host.
pub(super) fn is_native_host() -> bool { IS_HELPER.load(Ordering::Relaxed) }

pub fn follow_input_desktop() {
    if IS_HELPER.load(Ordering::Relaxed) {
        retarget_input_desktop();
    }
}

/// Puts the calling thread on whatever desktop currently receives input (the normal
/// desktop, the lock screen or a UAC prompt's secure desktop), so SendInput reaches it.
fn retarget_input_desktop() {
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, OpenInputDesktop, SetThreadDesktop,
    };
    use std::cell::Cell;
    thread_local! {
        static CURRENT: Cell<isize> = const { Cell::new(0) };
    }
    const GENERIC_ALL: u32 = 0x1000_0000;
    // SAFETY: the opened desktop replaces the thread's previous one, which is then closed.
    unsafe {
        let Ok(desk) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(GENERIC_ALL)) else {
            return;
        };
        if SetThreadDesktop(desk).is_ok() {
            let old = CURRENT.with(|c| c.replace(desk.0 as isize));
            if old != 0 && old != desk.0 as isize {
                let _ = CloseDesktop(windows::Win32::System::StationsAndDesktops::HDESK(old as *mut _));
            }
        } else {
            let _ = CloseDesktop(desk);
        }
    }
}

/// Ctrl+Alt+Del, as if a person pressed it. Windows only takes SendSAS from a service
/// process, so the helper signals the event the service handed it and the service calls it.
fn send_sas_now() {
    use windows::Win32::System::Threading::SetEvent;
    let event = std::env::args().nth(2).and_then(|h| h.parse::<isize>().ok());
    let Some(event) = event else {
        log_helper("secure helper: no Ctrl+Alt+Del event from the service");
        return;
    };
    // SAFETY: an event handle the service made inheritable and passed on our command line.
    if unsafe { SetEvent(windows::Win32::Foundation::HANDLE(event as *mut _)) }.is_err() {
        log_helper("secure helper: couldn't signal Ctrl+Alt+Del");
    }
}

/// A SECURITY_DESCRIPTOR allowing LocalSystem, Administrators and the interactively
/// logged-on console user, built from an SDDL string. Remote clients are rejected by the
/// pipe itself (PIPE_REJECT_REMOTE_CLIENTS).
///
/// The Interactive-Users ACE (IU, S-1-5-4) is what the per-user host actually matches: an
/// admin's ordinary host process runs unelevated, so its Administrators group is deny-only
/// and the BA ACE wouldn't grant it; IU grants the console user regardless of elevation,
/// and regardless of whether anyone was logged in when the helper (and its pipe) started.
fn security_descriptor() -> SecurityDescriptor {
    // Owned by SYSTEM explicitly (a SYSTEM process's objects default to Administrators), so
    // the host can tell the real helper's pipe from one another process made.
    let mut sddl = String::from("O:SYD:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)");
    if let Some(sid) = console_user_sid() {
        sddl.push_str(&format!("(A;;GRGW;;;{sid})"));
    }
    SecurityDescriptor::from_sddl(&sddl)
}

/// Owns a PSECURITY_DESCRIPTOR so it stays alive while the pipe is created.
struct SecurityDescriptor(windows::Win32::Security::PSECURITY_DESCRIPTOR);

// SAFETY: a plain owned allocation, only read while making the pipe.
unsafe impl Send for SecurityDescriptor {}

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> Self {
        use windows::Win32::Security::Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
        };
        let mut psd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
        // SAFETY: a NUL-terminated SDDL string; psd receives a LocalAlloc'd descriptor.
        unsafe {
            let _ = ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &windows::core::HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut psd,
                None,
            );
        }
        Self(psd)
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.0.is_null() {
            // SAFETY: freeing the LocalAlloc'd descriptor from the conversion above.
            unsafe { let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(self.0.0))); }
        }
    }
}

/// Creates the pipe instance, with the strict ACL and remote clients rejected.
fn create_pipe(sd: &SecurityDescriptor, name: &str) -> anyhow::Result<Pipe> {
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: a NUL-terminated name and a security descriptor that outlives this call.
    let handle = unsafe {
        CreateNamedPipeW(
            &windows::core::HSTRING::from(name),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            256 * 1024,
            256 * 1024,
            0,
            Some(&sa),
        )
    };
    if handle.is_invalid() {
        anyhow::bail!("CreateNamedPipeW failed; is another helper running?");
    }
    Ok(Pipe::from_handle(handle))
}

/// The SID string of the user signed in at the active console session, if any.
fn console_user_sid() -> Option<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
    // SAFETY: the token is closed before returning.
    unsafe {
        let session = WTSGetActiveConsoleSessionId();
        if session == 0xFFFF_FFFF {
            return None;
        }
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token).ok()?;
        let sid = token_user_sid(token);
        let _ = CloseHandle(token);
        sid
    }
}

fn log_helper(line: &str) {
    tracing::info!("{line}");
    let dir = super::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("secure.log")) {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let _ = writeln!(f, "{secs} {line}");
    }
}

/// What the helper says first: "GLASS" and the version, padded. Fixed size, so the host
/// reads exactly this much.
const HANDSHAKE_LEN: usize = 32;

fn handshake() -> [u8; HANDSHAKE_LEN] {
    let mut out = [0u8; HANDSHAKE_LEN];
    let text = format!("GLASS {}", env!("CARGO_PKG_VERSION"));
    let n = text.len().min(HANDSHAKE_LEN);
    out[..n].copy_from_slice(&text.as_bytes()[..n]);
    out
}

/// Logs a host-side problem with the helper once (the driver retries every few hundred ms).
fn warn_once(msg: &str) {
    static SAID: Mutex<Option<String>> = Mutex::new(None);
    let mut said = SAID.lock().unwrap();
    if said.as_deref() != Some(msg) {
        tracing::warn!("{msg}");
        *said = Some(msg.to_owned());
    }
}

/// Whether the pipe's owner is LocalSystem.
fn owned_by_system(handle: windows::Win32::Foundation::HANDLE) -> bool {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT};
    use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    // SAFETY: queries our own open handle; the descriptor is freed after reading the owner.
    unsafe {
        let mut owner = PSID::default();
        let mut sd = PSECURITY_DESCRIPTOR::default();
        let got = GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut sd),
        );
        if got.is_err() {
            return false;
        }
        let mut s = windows::core::PWSTR::null();
        let system = ConvertSidToStringSidW(owner, &mut s).is_ok() && s.to_string().is_ok_and(|s| s == "S-1-5-18");
        if !s.is_null() {
            let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        }
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        system
    }
}

/// In the helper: a connection is served only from this session's own signed-in user. The
/// pipe's ACL lets any interactive user open it (an unelevated host has no better group to
/// match), so another user, say over Remote Desktop, would otherwise be able to see and
/// drive this session's screen as SYSTEM.
fn client_check(handle: windows::Win32::Foundation::HANDLE) -> Result<(), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Pipes::{GetNamedPipeClientProcessId, GetNamedPipeClientSessionId};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::Security::TOKEN_QUERY;
    // SAFETY: queries about the connected client; every handle is closed before returning.
    unsafe {
        let (mut theirs, mut ours) = (0u32, 0u32);
        GetNamedPipeClientSessionId(handle, &mut theirs).map_err(|e| format!("no client session: {e}"))?;
        ProcessIdToSessionId(std::process::id(), &mut ours).map_err(|e| format!("no session: {e}"))?;
        if theirs != ours {
            return Err(format!("from session {theirs}, not {ours}"));
        }
        let mut pid = 0u32;
        GetNamedPipeClientProcessId(handle, &mut pid).map_err(|e| format!("no client process: {e}"))?;
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).map_err(|e| format!("client process: {e}"))?;
        let mut token = windows::Win32::Foundation::HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        let _ = CloseHandle(process);
        opened.map_err(|e| format!("client token: {e}"))?;
        let user = token_user_sid(token);
        let _ = CloseHandle(token);
        let console = console_user_sid();
        match (user, console) {
            (Some(u), Some(c)) if u == c => Ok(()),
            (u, c) => Err(format!("user {u:?}, not the console's {c:?}")),
        }
    }
}

/// The SID string of a token's user.
fn token_user_sid(token: windows::Win32::Foundation::HANDLE) -> Option<String> {
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_USER, TokenUser};
    // SAFETY: the buffer is sized by the first call; the string is freed after copying.
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).ok()?;
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = windows::core::PWSTR::null();
        ConvertSidToStringSidW(tu.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(s.0.cast())));
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn helper_text_packets_reassemble_unicode_in_order() {
        let packet = super::text_packet("@č\u{1f600}@č\u{1f600}");
        assert!(packet.len() > 1 + glass_proto::INPUT_EVENT_LEN);
        let mut reader = glass_proto::TextReader::default();
        let mut result = Vec::new();
        for record in packet.chunks_exact(1 + glass_proto::INPUT_EVENT_LEN) {
            assert_eq!(record[0], super::OP_INPUT);
            let event = glass_proto::InputEvent::read(record[1..].try_into().unwrap()).unwrap();
            if let Some(text) = reader.push(&event) {
                result.push(text);
            }
        }
        assert_eq!(result, ["@č\u{1f600}@č\u{1f600}"]);
        assert!(super::text_packet("").is_empty());
    }

    #[test]
    fn the_handshake_names_this_version() {
        let h = super::handshake();
        let text = String::from_utf8_lossy(&h);
        assert!(text.starts_with(&format!("GLASS {}", env!("CARGO_PKG_VERSION"))), "{text}");
        assert_eq!(h.len(), super::HANDSHAKE_LEN);
    }
}

// Native execution has a distinct bounded transport. Capture frame writes, even when
// stalled, cannot hold this pipe's writer or delay its completion acknowledgement.
const STEP_PIPE: &str = r"\\.\pipe\glass-secure-step-v1";
const STEP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(1);

pub struct StepClient { client: crate::input_protocol::Client<Pipe> }
impl crate::input_protocol::Transport for Pipe {
    fn exchange(&mut self, request: &[u8; crate::input_protocol::REQUEST_LEN], deadline: std::time::Instant) -> Result<[u8; crate::input_protocol::ACK_LEN], ()> {
        let mut reply = [0; crate::input_protocol::ACK_LEN];
        self.write_until(request, deadline).map_err(|_| ())?;
        self.read_until(&mut reply, deadline).map_err(|_| ())?;
        Ok(reply)
    }
    fn cancel(&mut self) { Pipe::cancel(self); }
}
impl StepClient {
    pub fn connect(epoch: u64) -> Option<Self> {
        let pipe = Pipe::open(STEP_PIPE).ok()?;
        if !owned_by_system(pipe.handle()) { return None; }
        let mut hello = [0; HANDSHAKE_LEN];
        pipe.read_until(&mut hello, std::time::Instant::now() + STEP_DEADLINE).ok()?;
        if hello != handshake() { return None; }
        Some(Self { client: crate::input_protocol::Client::new(pipe, epoch) })
    }
    pub fn execute(&mut self, operation: crate::input_queue::Operation) -> crate::text_input::Completion { self.client.execute(operation) }
}

fn run_step_server() -> anyhow::Result<()> {
    use windows::Win32::System::Pipes::DisconnectNamedPipe;
    loop {
        let sd = security_descriptor();
        let pipe = Arc::new(create_pipe(&sd, STEP_PIPE)?);
        if !pipe.connect() { continue; }
        if client_check(pipe.handle()).is_ok() { serve_steps(pipe.clone()); }
        let _ = unsafe { DisconnectNamedPipe(pipe.handle()) };
    }
}
fn serve_steps(pipe: Arc<Pipe>) {
    use crate::{input_protocol::{Request, Ack, REQUEST_LEN}, media::InputSink, text_input::Completion};
    if pipe.write_until(&handshake(), std::time::Instant::now() + STEP_DEADLINE).is_err() { return; }
    let session = Arc::new(crate::input_protocol::HelperSession::default());
    let (tx, rx) = std::sync::mpsc::sync_channel::<Request>(1);
    let worker_pipe = pipe.clone(); let stopped = session.clone();
    let worker = std::thread::spawn(move || {
        let mut injector = super::input::Injector::native_executor(0);
        let mut desktop = None;
        while let Ok(request) = rx.recv() {
            if !stopped.can_execute() { break; }
            let current = input_desktop_name();
            let changed = current.is_none() || desktop.as_ref().is_some_and(|old| Some(old) != current.as_ref());
            let completion = if changed {
                // Best effort on the previous executor before changing thread desktop.
                injector.release_all();
                Completion::Unknown
            } else {
                desktop = current;
                retarget_input_desktop();
                injector.execute_step(&request.operation)
            };
            // EOF takes effect out of band, even if a native call finished late.
            if !stopped.can_execute() { break; }
            let ack = Ack { epoch: request.epoch, sequence: request.sequence, completion };
            stopped.completed();
            if worker_pipe.write_until(&ack.encode(), std::time::Instant::now() + STEP_DEADLINE).is_err() { stopped.cancel(); worker_pipe.cancel(); break; }
            if completion == Completion::Unknown { stopped.cancel(); worker_pipe.cancel(); break; }
        }
        injector.release_all();
    });
    loop {
        let mut bytes = [0; REQUEST_LEN];
        if (&*pipe).read_exact(&mut bytes).is_err() { break; }
        let Some(request) = Request::decode(&bytes) else { break; };
        if !session.admit(&request) { break; }
        if tx.try_send(request).is_err() { break; }
    }
    session.cancel();
    pipe.cancel();
    drop(tx);
    let _ = worker.join();
}

/// Desktop identity is only an invalidation hint, never a foreground-window pin.
pub(super) fn input_desktop_name() -> Option<Vec<u16>> {
    use windows::Win32::{Foundation::HANDLE, System::StationsAndDesktops::{OpenInputDesktop, CloseDesktop, GetUserObjectInformationW, DESKTOP_CONTROL_FLAGS, DESKTOP_READOBJECTS, UOI_NAME}};
    unsafe {
        let desktop = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS).ok()?;
        let mut name = [0u16; 256];
        let mut needed = 0;
        let result = GetUserObjectInformationW(HANDLE(desktop.0), UOI_NAME, Some(name.as_mut_ptr().cast()), std::mem::size_of_val(&name) as u32, Some(&mut needed));
        let _ = CloseDesktop(desktop);
        result.ok()?;
        if needed == 0 || needed as usize > std::mem::size_of_val(&name) { return None; }
        Some(name[..needed as usize / 2].to_vec())
    }
}
