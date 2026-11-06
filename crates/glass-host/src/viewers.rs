//! Who's connected right now: shown in the macOS menu bar and announced on Linux and
//! Windows, so the person at the computer always knows when it's being viewed, and can end it.

use std::net::IpAddr;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use tokio::sync::watch;

/// How a viewer got in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum How {
    /// One of the owner's own tailnet devices.
    Tailnet,
    /// The access token.
    Token,
    /// A one-time link (or the pass it was traded for).
    Guest,
}

#[derive(Debug, Clone)]
pub struct Viewer {
    pub id: u64,
    /// "Chrome on Windows", "Safari on iPad", or a tailnet device name.
    pub device: String,
    pub how: How,
    /// Over the internet (WebRTC via glassdesk.dev) rather than the tailnet.
    pub remote: bool,
    pub peer: IpAddr,
    pub since: SystemTime,
}

impl Viewer {
    /// "Safari on iPad · remote guest from 203.0.113.9 · 3 min"
    pub fn describe(&self) -> String {
        let how = match (self.remote, self.how) {
            (true, How::Guest) => "remote guest",
            (true, _) => "remote",
            (false, How::Tailnet) => "your tailnet",
            (false, How::Token) => "token",
            (false, How::Guest) => "guest",
        };
        let mins = self.since.elapsed().map(|d| d.as_secs() / 60).unwrap_or(0);
        let ago = match mins {
            0 => "just now".to_string(),
            m if m < 60 => format!("{m} min"),
            m => format!("{} h {} min", m / 60, m % 60),
        };
        // Over the internet, the address says where it's from (worth seeing if unexpected).
        let from = if self.remote {
            format!(" from {}", self.peer)
        } else {
            String::new()
        };
        format!("{} · {how}{from} · {ago}", self.device)
    }
}

struct Entry {
    viewer: Viewer,
    conn: wtransport::Connection,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn channel() -> &'static watch::Sender<Vec<Viewer>> {
    static TX: OnceLock<watch::Sender<Vec<Viewer>>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(Vec::new()).0)
}

fn publish(entries: &[Entry]) {
    let list = entries.iter().map(|e| e.viewer.clone()).collect();
    channel().send_replace(list);
    crate::privacy::viewers_changed(entries.len());
}

/// The current viewers, and every change to them.
pub fn subscribe() -> watch::Receiver<Vec<Viewer>> {
    channel().subscribe()
}

/// Returns once nobody is connected.
pub async fn until_none() {
    let _ = subscribe().wait_for(|viewers| viewers.is_empty()).await;
}

/// Records a signed-in viewer until the returned guard drops (the session ends).
pub fn joined(viewer: Viewer, conn: wtransport::Connection) -> Leave {
    let id = viewer.id;
    tracing::info!("viewing: {}", viewer.describe());
    #[cfg(any(target_os = "linux", windows))]
    notify(&viewer);
    let mut entries = ENTRIES.lock().unwrap();
    entries.push(Entry { viewer, conn });
    publish(&entries);
    Leave(id)
}

pub struct Leave(u64);

impl Drop for Leave {
    fn drop(&mut self) {
        let mut entries = ENTRIES.lock().unwrap();
        entries.retain(|e| e.viewer.id != self.0);
        publish(&entries);
    }
}

/// Ends every session (their browsers show the reconnect screen; guests and remote viewers
/// need a new link if their passes were revoked too).
pub fn disconnect_all() {
    let entries = ENTRIES.lock().unwrap();
    for e in entries.iter() {
        e.conn.close(0u32.into(), b"disconnected by the host");
    }
    tracing::info!("disconnected {} viewer(s) from the host", entries.len());
}

/// Ends one session; false if it's gone already.
pub fn disconnect(id: u64) -> bool {
    let entries = ENTRIES.lock().unwrap();
    match entries.iter().find(|e| e.viewer.id == id) {
        Some(e) => {
            e.conn.close(0u32.into(), b"disconnected by the host");
            true
        }
        None => false,
    }
}

pub fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// A short, safe label from what the browser says it is.
pub fn clean_device(raw: Option<&str>) -> String {
    raw.map(|d| {
        d.chars()
            .filter(|c| !c.is_control())
            .take(48)
            .collect::<String>()
    })
    .map(|d| d.trim().to_string())
    .filter(|d| !d.is_empty())
    .unwrap_or_else(|| "A browser".into())
}

#[cfg(target_os = "linux")]
fn notify(viewer: &Viewer) {
    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=Glass",
            "Glass: someone is viewing this computer",
            &viewer.describe(),
        ])
        .spawn();
}

#[cfg(windows)]
fn notify(viewer: &Viewer) {
    crate::notify::notify("Glass: someone is viewing this computer", &viewer.describe());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewer(how: How, remote: bool) -> Viewer {
        Viewer {
            id: 1,
            device: "Safari on iPad".into(),
            how,
            remote,
            peer: "100.64.0.2".parse().unwrap(),
            since: SystemTime::now(),
        }
    }

    #[test]
    fn describes_viewers_plainly() {
        assert_eq!(
            viewer(How::Tailnet, false).describe(),
            "Safari on iPad · your tailnet · just now"
        );
        assert_eq!(
            viewer(How::Guest, true).describe(),
            "Safari on iPad · remote guest from 100.64.0.2 · just now"
        );
        let mut old = viewer(How::Token, false);
        old.since = SystemTime::now() - std::time::Duration::from_secs(3700);
        assert_eq!(old.describe(), "Safari on iPad · token · 1 h 1 min");
    }

    #[test]
    fn device_labels_are_cleaned() {
        assert_eq!(
            clean_device(Some("  Chrome on Windows ")),
            "Chrome on Windows"
        );
        assert_eq!(clean_device(Some("x\u{7}y")), "xy");
        assert_eq!(clean_device(None), "A browser");
        assert_eq!(clean_device(Some(&"a".repeat(100))).len(), 48);
    }
}
