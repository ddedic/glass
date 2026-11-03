//! A Unix socket the running host listens on (`glass.sock` in the config dir, owner-only;
//! on Windows a named pipe, `\\.\pipe\glass-<config dir hash>`, local clients only) so the CLI can
//! act on it: answer connection requests, chat, privacy mode, viewers.
//! One JSON request per line, one JSON reply per line.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Request {
    /// Who's connected.
    Viewers,
    /// End one session, or all of them.
    Disconnect { id: Option<u64> },
    /// Connections waiting to be allowed.
    Pending,
    Decide { id: u64, allow: bool },
    /// A message to every viewer.
    Chat { text: String },
    /// The conversation so far.
    ChatLog,
    /// Turn privacy mode on or off for the current sessions.
    Privacy { on: bool },
    /// An agent's status and its questions; with `op` (pause, resume, stop, allow), steer it.
    Agent {
        #[serde(default)]
        op: Option<String>,
    },
    /// Answer an agent's question.
    AgentAnswer {
        id: u64,
        ok: bool,
        #[serde(default)]
        note: Option<String>,
    },
    /// Windows: restart the host or stop the background launcher, asked from another
    /// session (an SSH shell), which the launcher's own signals don't reach.
    Launcher { op: String },
}

#[cfg(unix)]
pub fn path() -> PathBuf {
    crate::config_dir().join("glass.sock")
}

/// One pipe per user (and per config dir, so tests and `glass support` get their own).
#[cfg(windows)]
pub fn path() -> PathBuf {
    PathBuf::from(pipe_name_for(&crate::config_dir()))
}

/// The pipe a host with this config directory listens on.
#[cfg(windows)]
pub(crate) fn pipe_name_for(config_dir: &std::path::Path) -> String {
    let dir = config_dir.to_string_lossy().to_lowercase();
    let digest = ring::digest::digest(&ring::digest::SHA256, dir.as_bytes());
    let tag: String = digest.as_ref()[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\glass-{tag}")
}

/// How many viewers that host reports. `Ok(None)` means it is not running.
#[cfg(windows)]
pub(crate) fn viewer_count(config_dir: &std::path::Path) -> Result<Option<usize>> {
    use std::io::{BufRead, Write};
    let mut stream = match std::fs::OpenOptions::new().read(true).write(true).open(pipe_name_for(config_dir)) {
        Ok(stream) => stream,
        Err(e) if e.raw_os_error() == Some(2) || e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context("open the host pipe"),
    };
    let mut line = serde_json::to_string(&Request::Viewers)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    std::io::BufReader::new(&stream).read_line(&mut reply)?;
    let value: serde_json::Value = serde_json::from_str(&reply).context("no reply from Glass")?;
    if let Some(e) = value.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("{e}");
    }
    value["viewers"].as_array().map(|v| v.len()).context("no viewers in the reply").map(Some)
}

/// Serves requests until the host exits.
#[cfg(windows)]
pub fn serve() -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = path();
    let mut server = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&name)
        .with_context(|| format!("create {}", name.display()))?;
    tokio::spawn(async move {
        loop {
            if server.connect().await.is_err() {
                continue;
            }
            let next = match ServerOptions::new().reject_remote_clients(true).create(&name) {
                Ok(next) => next,
                Err(e) => {
                    tracing::warn!("ipc: {e}");
                    return;
                }
            };
            let stream = std::mem::replace(&mut server, next);
            let (read, write) = tokio::io::split(stream);
            tokio::spawn(serve_one(read, write));
        }
    });
    Ok(())
}

/// Serves requests until the host exits.
#[cfg(unix)]
pub fn serve() -> Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&p);
    let listener = tokio::net::UnixListener::bind(&p).with_context(|| format!("bind {}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let (read, write) = stream.into_split();
            tokio::spawn(serve_one(read, write));
        }
    });
    Ok(())
}

/// One client: a reply for each request line, until it hangs up.
async fn serve_one(read: impl tokio::io::AsyncRead + Unpin, mut write: impl tokio::io::AsyncWrite + Unpin) {
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let reply = match serde_json::from_str::<Request>(&line) {
            Ok(req) => handle(req).await,
            Err(e) => serde_json::json!({ "error": format!("bad request: {e}") }),
        };
        let mut out = reply.to_string();
        out.push('\n');
        if write.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

async fn handle(req: Request) -> serde_json::Value {
    use serde_json::json;
    match req {
        Request::Viewers => {
            let list: Vec<_> = crate::viewers::subscribe()
                .borrow()
                .iter()
                .map(|v| json!({ "id": v.id, "describe": v.describe(), "device": v.device }))
                .collect();
            json!({ "viewers": list })
        }
        Request::Disconnect { id } => {
            let n = match id {
                Some(id) => crate::viewers::disconnect(id) as usize,
                None => {
                    let n = crate::viewers::subscribe().borrow().len();
                    crate::viewers::disconnect_all();
                    n
                }
            };
            json!({ "disconnected": n })
        }
        Request::Pending => json!({ "pending": crate::approval::pending() }),
        Request::Decide { id, allow } => json!({ "ok": crate::approval::decide(id, allow) }),
        Request::Chat { text } => match crate::server::host_chat(&text) {
            Ok(n) => json!({ "sent": n }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
        Request::ChatLog => json!({ "messages": crate::server::chat_log() }),
        Request::Privacy { on } => match crate::privacy::set_forced(on) {
            Ok(()) => json!({ "privacy": on }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
        Request::Agent { op } => {
            if let Some(op) = op {
                crate::computer::control(&op);
            }
            json!({ "status": crate::computer::status(), "asks": crate::computer::asks(), "privacy": crate::privacy::active() })
        }
        Request::Launcher { op } => match launcher(&op) {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
        Request::AgentAnswer { id, ok, note } => {
            crate::computer::answer(id, crate::computer::Answer { ok, note });
            json!({ "ok": true })
        }
    }
}

#[cfg(windows)]
fn launcher(op: &str) -> Result<()> {
    crate::agent::from_host(op)
}

#[cfg(not(windows))]
fn launcher(op: &str) -> Result<()> {
    anyhow::bail!("`{op}` is for the Windows background launcher")
}

/// Sends one request to the running host.
pub fn call(req: &Request) -> Result<serde_json::Value> {
    use std::io::{BufRead, Write};
    let not_running = |_| anyhow::anyhow!("Glass isn't running here. Start it (glass setup), then try again.");
    #[cfg(unix)]
    let mut stream = {
        let stream = std::os::unix::net::UnixStream::connect(path()).map_err(not_running)?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
        stream
    };
    #[cfg(windows)]
    let mut stream = std::fs::OpenOptions::new().read(true).write(true).open(path()).map_err(not_running)?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    let value: serde_json::Value = serde_json::from_str(&reply).context("no reply from Glass")?;
    if let Some(e) = value.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("{e}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_one_json_line() {
        let s = serde_json::to_string(&Request::Decide { id: 3, allow: true }).unwrap();
        assert_eq!(s, r#"{"cmd":"decide","id":3,"allow":true}"#);
        let r: Request = serde_json::from_str(r#"{"cmd":"disconnect","id":null}"#).unwrap();
        assert!(matches!(r, Request::Disconnect { id: None }));
    }
}
