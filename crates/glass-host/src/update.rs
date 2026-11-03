//! Knowing about and installing new releases. The host checks glassdesk.dev/version.json
//! now and then; a client can ask the host to update itself, which re-runs the installer
//! in its own process group so it survives the service restart it triggers.
//!
//! An MSI install is not replaced by that installer. On Windows the secure-desktop service
//! installs the official GitHub MSI itself (see `windows::auto_update`), on the same
//! six-hour cadence, and only while the Updates policy allows it.

use std::sync::Mutex;
use std::time::Duration;

pub(crate) const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

/// GitHub's latest stable release of Ntarasiuk/glass. The MSI is downloaded from the
/// release asset URLs, never from this JSON body's download links.
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/Ntarasiuk/glass/releases/latest";

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
        return Some(
            "Glass is installed for everyone on this computer. The secure-desktop service installs updates; an administrator can also use the new MSI, or winget upgrade Glassdesk.Glass.",
        );
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
                        crate::push::activity(
                            "Glass update ready",
                            &format!("Version {v} is out (you have {CURRENT})."),
                            "update",
                        );
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
    anyhow::ensure!(
        status.success(),
        "the update didn't finish; see update.log in Glass's logs"
    );
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

/// What one Glass host said when asked who is connected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostViewers {
    /// That host is not running.
    Down,
    /// It answered with this many viewers.
    Count(usize),
    /// It might be serving someone, and we could not get an answer.
    Unknown,
}

/// Whether the service should install, wait, or do nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SilentDecision {
    Install {
        version: String,
    },
    /// A viewer may be connected. Try again later; do not close their session.
    Defer,
    Skip,
}

pub struct SilentInput<'a> {
    pub current: &'a str,
    pub latest: Option<&'a str>,
    pub updates_allowed: bool,
    pub machine_install: bool,
    pub viewers: &'a [HostViewers],
}

/// The MSI the Windows service may install. Per-user installs, a policy opt-out, and an
/// active (or unknown) viewer session never reach `Install`.
pub fn decide_silent(input: SilentInput<'_>) -> SilentDecision {
    if !input.updates_allowed || !input.machine_install {
        return SilentDecision::Skip;
    }
    let Some(latest) = input
        .latest
        .filter(|v| is_stable_version(v) && newer(v, input.current))
    else {
        return SilentDecision::Skip;
    };
    if session_blocks_update(input.viewers) {
        return SilentDecision::Defer;
    }
    SilentDecision::Install {
        version: latest.to_owned(),
    }
}

fn session_blocks_update(viewers: &[HostViewers]) -> bool {
    viewers.iter().any(|v| match v {
        HostViewers::Down | HostViewers::Count(0) => false,
        HostViewers::Count(_) | HostViewers::Unknown => true,
    })
}

/// `major.minor.patch` with no suffix and no leading zeros.
fn is_stable_version(v: &str) -> bool {
    let mut parts = v.split('.');
    let numeric = |s: &str| {
        !s.is_empty()
            && s.bytes().all(|b| b.is_ascii_digit())
            && (s.len() == 1 || !s.starts_with('0'))
    };
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(a), Some(b), Some(c), None) if numeric(a) && numeric(b) && numeric(c)
    )
}

/// The official asset URL. Anything that is not that exact MSI (or its `.sha256`) is refused.
pub fn official_asset_url(version: &str, file_name: &str) -> Option<String> {
    if !is_stable_version(version) || !is_release_asset(version, file_name) {
        return None;
    }
    Some(format!(
        "https://github.com/Ntarasiuk/glass/releases/download/v{version}/{file_name}"
    ))
}

fn is_release_asset(version: &str, file_name: &str) -> bool {
    ["x86_64", "arm64"].iter().any(|arch| {
        let msi = format!("glass-{version}-windows-{arch}.msi");
        file_name == msi || file_name == format!("{msi}.sha256")
    })
}

/// A redirect target GitHub uses for release files. The first request stays on the exact
/// `github.com/Ntarasiuk/glass` asset URL; only these hosts may serve the bytes.
pub fn release_cdn_url(url: &str) -> bool {
    const CDN: &[&str] = &[
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
        "github-releases.githubusercontent.com",
    ];
    https_host(url).is_some_and(|host| CDN.iter().any(|allowed| host.eq_ignore_ascii_case(allowed)))
}

fn https_host(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let hostport = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|s| !s.is_empty() && !s.contains('@'))?;
    let host = match hostport.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty()
                && !host.contains(':')
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            host
        }
        None => hostport,
        Some(_) => return None,
    };
    if host
        .bytes()
        .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
    {
        return None;
    }
    Some(host)
}

/// One Windows MSI chosen from a GitHub release document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedDownload {
    pub version: String,
    pub msi_name: String,
    pub msi_url: String,
    pub sha_url: String,
}

/// Reads GitHub's release JSON and builds the official URLs. Download links inside the
/// document are ignored, so a surprising `browser_download_url` cannot redirect the install.
pub fn plan_from_release_json(body: &str, arch: &str) -> Option<PlannedDownload> {
    if arch != "x86_64" && arch != "arm64" {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    if v["draft"].as_bool().unwrap_or(true) || v["prerelease"].as_bool().unwrap_or(true) {
        return None;
    }
    let version = v["tag_name"].as_str()?.strip_prefix('v')?;
    if !is_stable_version(version) {
        return None;
    }
    let msi_name = format!("glass-{version}-windows-{arch}.msi");
    let sha_name = format!("{msi_name}.sha256");
    let names: Vec<&str> = v["assets"]
        .as_array()?
        .iter()
        .filter_map(|a| a["name"].as_str())
        .collect();
    if !names.contains(&msi_name.as_str()) || !names.contains(&sha_name.as_str()) {
        return None;
    }
    Some(PlannedDownload {
        version: version.to_owned(),
        msi_name: msi_name.clone(),
        msi_url: official_asset_url(version, &msi_name)?,
        sha_url: official_asset_url(version, &sha_name)?,
    })
}

/// The published `.sha256` file (sha256sum: hex, two spaces, file name).
pub fn sha256_file_matches(bytes: &[u8], sha_text: &str, file_name: &str) -> bool {
    let mut lines = sha_text.lines().filter(|line| !line.trim().is_empty());
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return false;
    };
    let mut parts = line.split_whitespace();
    let (Some(hex), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let name = name.strip_prefix('*').unwrap_or(name);
    if name != file_name || hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return false;
    }
    hex.eq_ignore_ascii_case(&sha256_hex(bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The MSI path the service may execute: `<program data>\Glass\updates\<official file>`,
/// not a user profile, a temp directory, or a path that climbs out with `..`.
pub fn msi_destination_ok(program_data: &str, msi: &str, file_name: &str) -> bool {
    if !is_msi_file_name(file_name) {
        return false;
    }
    let Some(root) = split_path(program_data) else {
        return false;
    };
    let Some(file) = split_path(msi) else {
        return false;
    };
    if root
        .iter()
        .any(|part| USER_WRITABLE.contains(&part.as_str()))
    {
        return false;
    }
    if root.first().is_none_or(|drive| !is_drive(drive)) {
        return false;
    }
    let mut expect = root;
    expect.extend([
        "glass".to_owned(),
        "updates".to_owned(),
        file_name.to_ascii_lowercase(),
    ]);
    file == expect
}

/// `child` is a file or directory strictly inside `root`.
pub fn path_is_inside(root: &str, child: &str) -> bool {
    let (Some(root), Some(child)) = (split_path(root), split_path(child)) else {
        return false;
    };
    child.len() > root.len() && child.starts_with(&root[..])
}

const USER_WRITABLE: &[&str] = &["users", "appdata", "temp", "tmp", "public"];

fn is_msi_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("glass-") else {
        return false;
    };
    let Some((version, tail)) = rest.split_once("-windows-") else {
        return false;
    };
    let Some(arch) = tail.strip_suffix(".msi") else {
        return false;
    };
    is_stable_version(version) && (arch == "x86_64" || arch == "arm64")
}

fn split_path(path: &str) -> Option<Vec<String>> {
    let path = path
        .strip_prefix(r"\\?\")
        .or_else(|| path.strip_prefix("//?/"))
        .unwrap_or(path);
    if path.starts_with('\\') || path.starts_with('/') || path.contains('\0') {
        return None;
    }
    let parts: Vec<String> = path
        .split(['\\', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    if parts
        .iter()
        .any(|part| part == "." || part == ".." || part.contains(':') && !is_drive(part))
    {
        return None;
    }
    Some(parts)
}

fn is_drive(part: &str) -> bool {
    let bytes = part.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// A directory DACL whose only entries are SYSTEM and Administrators, both present.
pub fn sddl_locked_to_system_and_admins(sddl: &str) -> bool {
    let Some(aces) = dacl_aces(sddl) else {
        return false;
    };
    if aces.is_empty() {
        return false;
    }
    let mut system = false;
    let mut admins = false;
    for sid in aces {
        match sid.to_ascii_uppercase().as_str() {
            "SY" | "S-1-5-18" => system = true,
            "BA" | "S-1-5-32-544" => admins = true,
            _ => return false,
        }
    }
    system && admins
}

fn dacl_aces(sddl: &str) -> Option<Vec<&str>> {
    let start = sddl.find("D:")? + 2;
    let rest = &sddl[start..];
    let mut depth = 0i32;
    let mut end = rest.len();
    let mut aces = Vec::new();
    let mut ace_at = None;
    for (i, ch) in rest.char_indices() {
        match ch {
            '(' => {
                if depth == 0 {
                    ace_at = Some(i + 1);
                }
                depth += 1;
            }
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                if depth == 0 {
                    let sid = allow_sid(&rest[ace_at?..i])?;
                    aces.push(sid);
                }
            }
            _ if depth == 0 && rest[i..].starts_with("S:") => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    if depth != 0 || end < rest.len() && !rest[end..].starts_with("S:") {
        return None;
    }
    Some(aces)
}

/// An allow ACE's account SID. A deny, or any other ACE, rejects the whole DACL.
fn allow_sid(ace: &str) -> Option<&str> {
    let mut fields = ace.split(';');
    let kind = fields.next()?;
    let _flags = fields.next()?;
    let _rights = fields.next()?;
    let _object = fields.next()?;
    let _inherit = fields.next()?;
    let sid = fields.next()?;
    if fields.next().is_some() || kind != "A" || sid.is_empty() {
        return None;
    }
    Some(sid)
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
    use super::*;

    fn input<'a>(
        latest: Option<&'a str>,
        allowed: bool,
        machine: bool,
        viewers: &'a [HostViewers],
    ) -> SilentInput<'a> {
        SilentInput {
            current: "0.13.1",
            latest,
            updates_allowed: allowed,
            machine_install: machine,
            viewers,
        }
    }

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn installs_a_newer_machine_release_only_when_nobody_is_connected() {
        let idle = [HostViewers::Down, HostViewers::Count(0)];
        assert_eq!(
            decide_silent(input(Some("0.13.2"), true, true, &idle)),
            SilentDecision::Install {
                version: "0.13.2".into()
            }
        );
        assert_eq!(
            decide_silent(input(Some("0.13.1"), true, true, &idle)),
            SilentDecision::Skip
        );
        assert_eq!(
            decide_silent(input(Some("0.13.0"), true, true, &idle)),
            SilentDecision::Skip
        );
        assert_eq!(
            decide_silent(input(None, true, true, &idle)),
            SilentDecision::Skip
        );
        assert_eq!(
            decide_silent(input(Some("0.13.2-windows.1"), true, true, &idle)),
            SilentDecision::Skip
        );
        assert_eq!(
            decide_silent(input(Some("0.13.2"), true, false, &idle)),
            SilentDecision::Skip,
            "per-user install"
        );
    }

    #[test]
    fn the_updates_policy_is_the_opt_out() {
        let idle = [HostViewers::Count(0)];
        assert_eq!(
            decide_silent(input(Some("9.0.0"), false, true, &idle)),
            SilentDecision::Skip
        );
    }

    #[test]
    fn a_connected_or_unknown_viewer_defers_the_install() {
        assert_eq!(
            decide_silent(input(Some("0.13.2"), true, true, &[HostViewers::Count(1)])),
            SilentDecision::Defer
        );
        assert_eq!(
            decide_silent(input(
                Some("0.13.2"),
                true,
                true,
                &[HostViewers::Count(0), HostViewers::Unknown]
            )),
            SilentDecision::Defer
        );
        assert_eq!(
            decide_silent(input(Some("0.13.2"), false, true, &[HostViewers::Count(2)])),
            SilentDecision::Skip,
            "opt-out wins over deferral"
        );
    }

    #[test]
    fn the_msi_url_is_the_official_github_release() {
        let msi = "glass-0.13.2-windows-x86_64.msi";
        assert_eq!(
            official_asset_url("0.13.2", msi).as_deref(),
            Some(
                "https://github.com/Ntarasiuk/glass/releases/download/v0.13.2/glass-0.13.2-windows-x86_64.msi"
            )
        );
        assert!(official_asset_url("0.13.2", "glass-0.13.2-windows-arm64.msi.sha256").is_some());
        assert!(official_asset_url("0.13.2", "glass-0.13.2-windows-x86_64.zip").is_none());
        assert!(official_asset_url("../0.13.2", msi).is_none());
        assert!(official_asset_url("0.13.2", "../glass.msi").is_none());
        assert_eq!(
            LATEST_RELEASE_URL,
            "https://api.github.com/repos/Ntarasiuk/glass/releases/latest"
        );
        assert!(release_cdn_url(
            "https://release-assets.githubusercontent.com/github-production-release-asset/1"
        ));
        assert!(!release_cdn_url(
            "https://github.com/Ntarasiuk/glass/releases/download/v0.13.2/glass.msi"
        ));
        assert!(!release_cdn_url("https://evil.example/glass.msi"));
        assert!(!release_cdn_url(
            "http://release-assets.githubusercontent.com/x"
        ));
        assert!(!release_cdn_url(
            "https://user@release-assets.githubusercontent.com/x"
        ));
        assert!(!release_cdn_url(
            "https://release-assets.githubusercontent.com.evil/x"
        ));
    }

    #[test]
    fn release_json_ignores_download_links_and_unwanted_builds() {
        let body = r#"{
            "tag_name": "v0.13.2",
            "draft": false,
            "prerelease": false,
            "assets": [
                {"name": "glass-0.13.2-windows-x86_64.msi", "browser_download_url": "https://evil.example/glass.msi"},
                {"name": "glass-0.13.2-windows-x86_64.msi.sha256", "browser_download_url": "https://evil.example/glass.sha256"}
            ]
        }"#;
        let plan = plan_from_release_json(body, "x86_64").unwrap();
        assert_eq!(plan.version, "0.13.2");
        assert_eq!(
            plan.msi_url,
            official_asset_url("0.13.2", &plan.msi_name).unwrap()
        );
        assert!(!plan.msi_url.contains("evil.example"));
        assert!(plan_from_release_json(body, "arm64").is_none());
        let pre = body.replace("\"prerelease\": false", "\"prerelease\": true");
        assert!(plan_from_release_json(&pre, "x86_64").is_none());
        assert!(
            plan_from_release_json(&body.replace("v0.13.2", "v0.13.2-windows.1"), "x86_64")
                .is_none()
        );
    }

    #[test]
    fn the_published_checksum_must_name_the_msi() {
        let bytes = b"abc";
        let sum = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let name = "glass-0.13.2-windows-x86_64.msi";
        assert!(sha256_file_matches(
            bytes,
            &format!("{sum}  {name}\n"),
            name
        ));
        assert!(sha256_file_matches(
            bytes,
            &format!("{sum} *{name}\n"),
            name
        ));
        let wrong = "0000000000000000000000000000000000000000000000000000000000000000";
        assert!(!sha256_file_matches(
            bytes,
            &format!("{wrong}  {name}\n"),
            name
        ));
        assert!(!sha256_file_matches(
            bytes,
            &format!("{sum}  other.msi\n"),
            name
        ));
        assert!(!sha256_file_matches(bytes, "", name));
    }

    #[test]
    fn the_msi_must_sit_in_the_programdata_updates_directory() {
        let name = "glass-0.13.2-windows-x86_64.msi";
        let ok = r"C:\ProgramData\Glass\updates\glass-0.13.2-windows-x86_64.msi";
        assert!(msi_destination_ok(r"C:\ProgramData", ok, name));
        assert!(msi_destination_ok(r"C:\ProgramData\", ok, name));
        assert!(!msi_destination_ok(
            r"C:\ProgramData",
            r"C:\Users\Public\Glass\updates\glass-0.13.2-windows-x86_64.msi",
            name
        ));
        assert!(!msi_destination_ok(
            r"C:\Users\Public",
            r"C:\Users\Public\Glass\updates\glass-0.13.2-windows-x86_64.msi",
            name
        ));
        assert!(!msi_destination_ok(
            r"C:\ProgramData",
            r"C:\ProgramData\Glass\updates\..\..\Temp\glass-0.13.2-windows-x86_64.msi",
            name
        ));
        assert!(!msi_destination_ok(
            r"C:\Windows\Temp",
            r"C:\Windows\Temp\Glass\updates\glass-0.13.2-windows-x86_64.msi",
            name
        ));
        assert!(!msi_destination_ok(
            r"\\server\share",
            r"\\server\share\Glass\updates\glass-0.13.2-windows-x86_64.msi",
            name
        ));
        assert!(!msi_destination_ok(r"C:\ProgramData", ok, "glass.msi"));
        assert!(path_is_inside(
            r"C:\Program Files\Glass",
            r"\\?\C:\Program Files\Glass\bin\glass.exe"
        ));
        assert!(!path_is_inside(
            r"C:\Program Files\Glass",
            r"C:\Program Files\GlassService\glass.exe"
        ));
    }

    #[test]
    fn the_updates_directory_acl_allows_only_system_and_administrators() {
        assert!(sddl_locked_to_system_and_admins(
            "O:SYG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        ));
        assert!(sddl_locked_to_system_and_admins(
            "D:(A;;FA;;;S-1-5-18)(A;;FA;;;S-1-5-32-544)"
        ));
        assert!(!sddl_locked_to_system_and_admins(
            "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FR;;;BU)"
        ));
        assert!(!sddl_locked_to_system_and_admins("D:(A;;FA;;;WD)"));
        assert!(!sddl_locked_to_system_and_admins("D:(A;;FA;;;SY)"));
        assert!(!sddl_locked_to_system_and_admins(
            "D:(D;;FA;;;SY)(A;;FA;;;BA)"
        ));
        assert!(!sddl_locked_to_system_and_admins(""));
    }
}
