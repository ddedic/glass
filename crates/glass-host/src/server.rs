//! WebTransport session handling plus the HTTP(S) server for the web client.

use std::net::{IpAddr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Json;
use axum::http::{HeaderValue, header};
use axum::routing::{get, post};
use glass_proto::{
    INPUT_EVENT_LEN, InputEvent, KIND_KEYFRAME_REQUEST, KIND_MIC, KIND_PING, MAX_STREAMS,
};
use tokio::sync::{broadcast, mpsc};
use tower_http::services::ServeDir;
use tower_http::set_header::SetResponseHeaderLayer;
use tracing::{info, warn};
use wtransport::config::Ipv6DualStackConfig;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::endpoint::IncomingSession;
use wtransport::tls::Sha256DigestFmt;
use wtransport::{Endpoint, Identity, RecvStream, SendStream, ServerConfig};

use crate::audio::AudioPackets;
use crate::clock::now_us;
use crate::control::{self, ClientMsg, STREAM_CLIPBOARD, STREAM_FILE, STREAM_INPUT, ServerMsg};
use crate::files;
use crate::media::EncodedFrame;
use crate::media::{Clipboard, InputSink, Layout, VideoSource};
use crate::tailscale;

pub struct Shared {
    pub frames: broadcast::Sender<Arc<EncodedFrame>>,
    pub injector: Mutex<Box<dyn InputSink>>,
    pub video: Arc<dyn VideoSource>,
    pub clipboard: Arc<dyn Clipboard>,
    /// Host-wide events fanned out to every authenticated session's control stream.
    pub events: broadcast::Sender<ServerMsg>,
    /// Read on every sign-in, so `glass token --reset` takes effect without a restart.
    pub token_file: PathBuf,
    /// Failed sign-ins per address, for brute-force protection.
    pub auth_failures:
        Mutex<std::collections::HashMap<std::net::IpAddr, (u32, std::time::Instant)>>,
    /// Tailnet login of this computer's owner; their devices skip the token.
    /// Updated after startup. Empty means unknown, tagged, or signed out: a token is required.
    pub tailnet_owner: tailscale::TailnetOwner,
    pub audio: AudioPackets,
    pub rate: Mutex<crate::rate::RateControl>,
}

pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub async fn run(
    shared: Arc<Shared>,
    web_dir: PathBuf,
    // The usual web port, where other machines' hosts are probed.
    http_port: u16,
    http_listeners: Vec<std::net::TcpListener>,
    https_listeners: Vec<std::net::TcpListener>,
    listen: crate::listen::Listen,
    wt_port: u16,
    wt_host: Option<String>,
    tls: Option<TlsFiles>,
    client_log: PathBuf,
    extra_routes: Option<axum::Router>,
) -> Result<()> {
    let identity = match &tls {
        Some(t) => Identity::load_pemfiles(&t.cert, &t.key).await?,
        None => self_signed_identity(wt_host.as_deref()).await?,
    };
    // Only meaningful for the self-signed identity; a real cert validates normally.
    let cert_hash = tls.is_none().then(|| {
        identity.certificate_chain().as_slice()[0]
            .hash()
            .fmt(Sha256DigestFmt::BytesArray)
    });

    // The usual port, or the next free one: clients learn it from /config.json.
    // Every listen address shares that port. The default is one dual-stack socket.
    let (endpoints, wt_port) = bind_wt(&identity, &listen, wt_port)?;
    let wt_where = match &listen {
        crate::listen::Listen::Default => "0.0.0.0 and [::]".to_string(),
        other => crate::listen::describe(other),
    };
    info!("WebTransport listening on {wt_where} udp/{wt_port}");
    crate::remote::watch(wt_port, listen.wt_dial());
    // Fleet's scripts and the microphone belong to a signed-in user, not the sign-in screen.
    if !crate::signin::active() {
        crate::fleet::watch();
        // What the computer is playing, for viewers' media controls (while anyone is connected).
        crate::now_playing::watch(shared.events.clone());
        // A mic left on when the host last stopped left BlackHole as the default input.
        crate::mic::recover();
    }

    let cert_hash = cert_hash.map(|h| serde_json::from_str::<Vec<u8>>(&h).unwrap());
    let version_dir = web_dir.clone();
    let client_config = move || {
        let (wt_host, cert_hash) = (wt_host.clone(), cert_hash.clone());
        let version = web_version(&version_dir);
        async move {
            Json(serde_json::json!({
                "wtPort": wt_port,
                "wtHost": wt_host,
                "certHash": cert_hash,
                "version": version,
                // For Wake-on-LAN from another host on this LAN.
                "macs": crate::wol::own_macs(),
                // The sign-in screen offers single sign-on.
                "sso": crate::oidc::enabled(),
                "ldap": crate::ldap::enabled(),
            }))
        }
    };
    let _ = SHARED.set(shared.clone());
    let _ = CLIENT_LOG.set(client_log.clone());
    let machines_cache: MachinesCache = Arc::new(tokio::sync::Mutex::new(None));
    let manifest_cache = machines_cache.clone();
    let manifest_dir = web_dir.clone();
    // The sign-in host (SYSTEM) serves the viewer and nothing else: forwarded ports, the
    // admin console, single sign-on, agents and file links would all act as SYSTEM.
    let extras = if crate::signin::active() {
        axum::Router::new()
    } else {
        axum::Router::new()
            .merge(crate::forward::routes())
            .merge(crate::oidc::routes())
            .merge(crate::admin::routes())
            .merge(crate::ldap::routes())
            .merge(crate::scim::routes())
            .merge(crate::acl::routes())
            .merge(crate::computer::routes())
            .merge(crate::oauth::routes())
            .merge(crate::push::routes())
            .merge(crate::browse::routes())
    };
    let app = axum::Router::new()
        .merge(extra_routes.unwrap_or_default())
        .merge(extras)
        .route("/config.json", get(client_config))
        .route(
            "/manifest.webmanifest",
            get(move || manifest(manifest_dir.clone(), manifest_cache.clone(), http_port, wt_port)),
        )
        .route(
            "/machines.json",
            get(move || machines(machines_cache.clone(), http_port, wt_port)),
        )
        .route(
            "/log",
            post(move |body: String| append_client_log(client_log.clone(), body)),
        )
        .fallback_service(ServeDir::new(web_dir))
        // The client is tiny and changes often; never let a stale worker linger.
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ));
    // Agents from anywhere: only the OAuth and MCP routes, on this computer alone, for
    // Tailscale Funnel to publish (glass agents public on). This listener is loopback-only
    // and does not follow --bind.
    if !crate::signin::active() {
        crate::oauth::serve_public();
    }
    let web_port = http_listeners
        .first()
        .context("no web listener")?
        .local_addr()?
        .port();
    let scheme = if tls.is_some() { "https" } else { "http" };
    if !https_listeners.is_empty() {
        info!(
            "web client at https://<host>/ (port {})",
            https_listeners[0].local_addr()?.port()
        );
    }
    info!("web client at {scheme}://<host>:{web_port}");
    let rustls = match &tls {
        Some(t) => {
            Some(axum_server::tls_rustls::RustlsConfig::from_pem_file(&t.cert, &t.key).await?)
        }
        None => None,
    };
    spawn_web(https_listeners, app.clone(), rustls.clone());
    spawn_web(http_listeners, app, rustls);

    loop {
        let (incoming, _, _) =
            futures_util::future::select_all(endpoints.iter().map(|ep| Box::pin(ep.accept()))).await;
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = session(incoming, shared).await {
                info!("session ended: {e:#}");
            }
        });
    }
}

fn spawn_web(
    listeners: Vec<std::net::TcpListener>,
    app: axum::Router,
    tls: Option<axum_server::tls_rustls::RustlsConfig>,
) {
    for listener in listeners {
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        if let Err(e) = listener.set_nonblocking(true) {
            warn!("port {port}: {e}");
            continue;
        }
        let app = app.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            let result = serve_one(listener, app, tls).await;
            if let Err(e) = result {
                tracing::error!("web server on port {port} stopped: {e}");
            }
        });
    }
}

async fn serve_one(
    listener: std::net::TcpListener,
    app: axum::Router,
    tls: Option<axum_server::tls_rustls::RustlsConfig>,
) -> Result<()> {
    let service = app.into_make_service_with_connect_info::<SocketAddr>();
    match tls {
        Some(cfg) => {
            axum_server::from_tcp_rustls(listener, cfg)?
                .serve(service)
                .await?;
        }
        None => {
            axum_server::from_tcp(listener)?.serve(service).await?;
        }
    }
    Ok(())
}

enum WtBind {
    Default,
    Address(IpAddr),
}

enum WtBindError {
    /// The address isn't assigned on this machine; another port won't help.
    AddrNotAvailable(String),
    /// Port taken, or another error worth trying the next port for.
    Unavailable(std::io::Error),
    Config(anyhow::Error),
}

fn bind_wt(
    identity: &Identity,
    listen: &crate::listen::Listen,
    first_port: u16,
) -> Result<(Vec<Endpoint<Server>>, u16)> {
    let last = first_port.saturating_add(crate::PORT_TRIES - 1);
    let mut last_err = None;
    for port in first_port..=last {
        match bind_wt_endpoints(identity, listen, port) {
            Ok(endpoints) => return Ok((endpoints, port)),
            Err(WtBindError::AddrNotAvailable(msg)) => anyhow::bail!("{msg}"),
            Err(WtBindError::Config(err)) => return Err(err),
            Err(WtBindError::Unavailable(err)) => {
                warn!("udp/{port} unavailable ({err}); trying the next port");
                last_err = Some(err);
            }
        }
    }
    let why = last_err.map(|e| format!(" ({e})")).unwrap_or_default();
    anyhow::bail!("UDP ports {first_port}–{last} are all in use{why}; free one or pass --wt-port")
}

fn bind_wt_endpoints(
    identity: &Identity,
    listen: &crate::listen::Listen,
    port: u16,
) -> Result<Vec<Endpoint<Server>>, WtBindError> {
    let specs: Vec<WtBind> = match listen {
        crate::listen::Listen::Default => vec![WtBind::Default],
        crate::listen::Listen::Explicit(ips) => ips.iter().copied().map(WtBind::Address).collect(),
    };
    let mut endpoints = Vec::with_capacity(specs.len());
    for spec in specs {
        let config = wt_server_config(identity, port, &spec)?;
        match Endpoint::server(config) {
            Ok(endpoint) => endpoints.push(endpoint),
            Err(err) if err.kind() == std::io::ErrorKind::AddrNotAvailable => {
                return Err(WtBindError::AddrNotAvailable(format!(
                    "can't listen on udp/{port} ({err}). Check --bind or `glass config bind`."
                )));
            }
            Err(err) => return Err(WtBindError::Unavailable(err)),
        }
    }
    Ok(endpoints)
}

fn wt_server_config(
    identity: &Identity,
    port: u16,
    spec: &WtBind,
) -> Result<ServerConfig, WtBindError> {
    let builder = ServerConfig::builder();
    let builder = match spec {
        WtBind::Default => builder.with_bind_default(port),
        WtBind::Address(IpAddr::V4(ip)) => {
            builder.with_bind_address(SocketAddr::new(IpAddr::V4(*ip), port))
        }
        WtBind::Address(IpAddr::V6(ip)) => builder.with_bind_address_v6(
            SocketAddrV6::new(*ip, port, 0, 0),
            // IPv4 has its own socket when it's in the list. Dual-stack would take that port.
            Ipv6DualStackConfig::Deny,
        ),
    };
    let configured = builder
        .with_identity(identity.clone_identity())
        .keep_alive_interval(Some(Duration::from_secs(2)))
        .max_idle_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| WtBindError::Config(anyhow::anyhow!("WebTransport idle timeout: {err:?}")))?;
    Ok(configured.build())
}

async fn session(incoming: IncomingSession, shared: Arc<Shared>) -> Result<()> {
    let request = incoming.await?;
    // A WebRTC viewer arrives through the local bridge, which vouches for its real address.
    let relayed = crate::remote::relayed_peer(request.path());
    let conn = request.accept().await?;
    let peer = conn.remote_address();
    match &relayed {
        Some(r) => info!(
            "client connected: {} (remote, over WebRTC{})",
            r.peer,
            if r.rtp_video { ", video as RTP" } else { "" }
        ),
        None => info!("client connected: {peer}"),
    }
    // Its video goes out as RTP tracks (rtp.rs), not as datagrams on this session.
    let rtp_video = relayed.as_ref().is_some_and(|r| r.rtp_video);

    let authed = Arc::new(AtomicBool::new(false));
    let audio_on = Arc::new(AtomicBool::new(false));
    let video_on = Arc::new(AtomicBool::new(true));
    let perms = Arc::new(std::sync::OnceLock::new());
    let mic_on = Arc::new(AtomicBool::new(false));
    let mut audio = shared.audio.subscribe();
    let (ctl_tx, ctl_rx) = mpsc::unbounded_channel::<ServerMsg>();
    let mut ctl_rx = Some(ctl_rx);
    let mut frames = shared.frames.subscribe();
    // Per stream, so a client sees a gap only when that stream lost a frame.
    let mut next_frame_id = [0u32; MAX_STREAMS];
    // When this client last had a keyframe request honoured: [all streams, stream 0, …].
    let mut last_keyframe_request = [None::<std::time::Instant>; MAX_STREAMS + 1];

    // The page pings every 500 ms; a session that stops hearing from it is a page that's
    // gone (Safari can keep a dead page's connection alive), or one suspended in the
    // background, which reconnects when it's back.
    let mut last_heard = std::time::Instant::now();
    let mut quiet_check = tokio::time::interval(QUIET_CHECK);

    let result: Result<()> = async {
        loop {
            tokio::select! {
                _ = quiet_check.tick() => {
                    if last_heard.elapsed() > QUIET_LIMIT {
                        bail!("viewer went quiet for {}s", QUIET_LIMIT.as_secs());
                    }
                }
                frame = frames.recv() => {
                    let frame = match frame {
                        Ok(f) => f,
                        // Fell behind: skip the ids too, so the client sees the gap and
                        // requests a keyframe instead of decoding a delta against a frame it
                        // never got.
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            warn!("{peer}: dropped {n} frames");
                            for id in &mut next_frame_id {
                                *id = id.wrapping_add(n as u32);
                            }
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    };
                    if rtp_video {
                        continue;
                    }
                    let Some(next_id) = next_frame_id.get_mut(frame.stream as usize) else { continue };
                    if authed.load(Ordering::Relaxed) && video_on.load(Ordering::Relaxed) {
                        send_frame(&conn, &frame, next_id)?;
                    } else {
                        // Keep ids moving so a resumed client sees a gap and waits for a keyframe.
                        *next_id = next_id.wrapping_add(1);
                    }
                }
                packet = audio.recv() => {
                    match packet {
                        Ok(p) if audio_on.load(Ordering::Relaxed) => {
                            conn.send_datagram(p.as_slice())?;
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    }
                }
                dgram = conn.receive_datagram() => {
                    let dgram = dgram?;
                    if crate::testmode::deaf() {
                        continue;
                    }
                    last_heard = std::time::Instant::now();
                    match dgram.first() {
                        Some(&KIND_PING) => {
                            if let Some(pong) = glass_proto::pong(&dgram, now_us()) {
                                conn.send_datagram(pong)?;
                            }
                        }
                        Some(&KIND_KEYFRAME_REQUEST) if authed.load(Ordering::Relaxed) => {
                            // Keyframes are big: on a lossy link, honouring every request
                            // makes the loss worse. One per stream per interval is enough.
                            let stream = dgram.get(1).copied();
                            let slot = stream.map_or(0, |s| (s as usize + 1).min(MAX_STREAMS));
                            let now = std::time::Instant::now();
                            if last_keyframe_request[slot].is_none_or(|t| now.duration_since(t) >= KEYFRAME_REQUEST_SPACING) {
                                last_keyframe_request[slot] = Some(now);
                                shared.video.request_keyframe(stream);
                            }
                        }
                        Some(&KIND_MIC) if mic_on.load(Ordering::Relaxed) => crate::mic::packet(&dgram),
                        _ => {}
                    }
                }
                stream = conn.accept_bi() => {
                    let (send, recv) = stream?;
                    // One control stream per session; extra ones are ignored.
                    let Some(rx) = ctl_rx.take() else { continue };
                    let ctx = SessionCtx {
                        conn: conn.clone(),
                        shared: shared.clone(),
                        authed: authed.clone(),
                        audio_on: audio_on.clone(),
                        video_on: video_on.clone(),
                        ctl_tx: ctl_tx.clone(),
                        relayed: relayed.clone(),
                        perms: perms.clone(),
                        mic_on: mic_on.clone(),
                    };
                    tokio::spawn(async move {
                        if let Err(e) = control(ctx, send, recv, rx).await {
                            info!("{peer}: control ended: {e:#}");
                        }
                    });
                }
                stream = conn.accept_uni() => {
                    let stream = stream?;
                    let ctx = SessionCtx {
                        conn: conn.clone(),
                        shared: shared.clone(),
                        authed: authed.clone(),
                        audio_on: audio_on.clone(),
                        video_on: video_on.clone(),
                        ctl_tx: ctl_tx.clone(),
                        relayed: relayed.clone(),
                        perms: perms.clone(),
                        mic_on: mic_on.clone(),
                    };
                    tokio::spawn(async move {
                        if let Err(e) = uni_stream(ctx, stream).await {
                            warn!("{peer}: stream failed: {e:#}");
                        }
                    });
                }
            }
        }
    }
    .await;
    // Ending here (gone quiet, an error) must end the whole session: the control stream's
    // task holds the connection, and with it this viewer's place in the list.
    conn.close(0u32.into(), b"session ended");

    // Anything still held, when no one else is giving input: another viewer may be holding
    // Shift right now (each input stream lets go of its own keys when it ends).
    if authed.load(Ordering::Relaxed) && INPUT_STREAMS.load(Ordering::Relaxed) == 0 {
        shared.injector.lock().unwrap().release_all();
        crate::gamepad::release_all();
    }
    info!("client disconnected: {peer}");
    result
}

#[derive(Clone)]
struct SessionCtx {
    conn: wtransport::Connection,
    shared: Arc<Shared>,
    authed: Arc<AtomicBool>,
    audio_on: Arc<AtomicBool>,
    video_on: Arc<AtomicBool>,
    ctl_tx: mpsc::UnboundedSender<ServerMsg>,
    /// The real address of a WebRTC viewer (the connection itself is loopback).
    relayed: Option<crate::remote::Relay>,
    /// What it may do, set once signed in.
    perms: Arc<std::sync::OnceLock<control::Perms>>,
    mic_on: Arc<AtomicBool>,
}

async fn control(
    ctx: SessionCtx,
    mut send: SendStream,
    mut recv: RecvStream,
    mut ctl_rx: mpsc::UnboundedReceiver<ServerMsg>,
) -> Result<()> {
    let shared = &ctx.shared;
    let hello = control::read_json::<ClientMsg>(&mut recv).await;
    let peer = ctx
        .relayed
        .as_ref()
        .map(|r| r.peer)
        .unwrap_or_else(|| ctx.conn.remote_address().ip().to_canonical());
    if auth_blocked(shared, peer) {
        warn!("{peer}: rejected (too many failed sign-ins)");
        control::write_json(
            &mut send,
            &ServerMsg::Error {
                msg: "unauthorized".into(),
            },
        )
        .await?;
        // Let the message arrive before closing, so the client shows the token prompt.
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"too many attempts");
        return Ok(());
    }
    let hello_ok = matches!(hello, Ok(Some(ClientMsg::Hello { .. })));
    let (token, link, pass, hello_device, codecs, ua) = match hello {
        Ok(Some(ClientMsg::Hello {
            token,
            link,
            pass,
            device,
            codecs,
            ua,
        })) => (token, link, pass, device, codecs, ua),
        _ => (None, None, None, None, Vec::new(), None),
    };
    let mut device = crate::viewers::clean_device(hello_device.as_deref());
    let remote = ctx.relayed.is_some();
    let refused = |device: &str, detail: &str| {
        crate::connlog::append(&crate::connlog::Entry {
            start: crate::access::now(),
            secs: 0,
            device: device.to_string(),
            how: "refused".into(),
            remote,
            peer: peer.to_string(),
            detail: detail.into(),
        });
    };
    let expected = control::load_or_create_token(&shared.token_file).unwrap_or_default();
    let access_file = crate::access::file_for(&shared.token_file);
    // Owner credentials first, so a tailnet device opening a link doesn't use it up.
    let mut auth = None;
    if !expected.is_empty()
        && token
            .as_deref()
            .is_some_and(|t| control::token_matches(t, &expected))
    {
        auth = Some(Auth::Token);
    }
    if auth.is_none()
        && hello_ok
        // Over WebRTC the address is public, never a tailnet device's.
        && ctx.relayed.is_none()
        && let Some(login) = tailscale::identity(peer).await
    {
        if shared.tailnet_owner.resolve().await.as_deref() == Some(login.as_str()) {
            auth = Some(Auth::Tailnet);
        } else if let Some(scope) = tailnet_guest_scope(&login) {
            // Someone else on the tailnet: the access rules name them, or the owner lets the
            // whole tailnet in (tailnet-members).
            auth = Some(Auth::TailnetGuest { login, scope });
        }
    }
    // A policy can turn guest links (or, in its own host, quick support) off: new links and
    // devices already let in by one are refused.
    let policy = crate::policy::load();
    if auth.is_none()
        && !policy.allows(if crate::support::is_host() { "support" } else { "links" })
        && (link.is_some()
            || pass
                .as_deref()
                .and_then(|p| crate::access::check_pass(&access_file, p))
                .is_some_and(|v| v.from_link))
    {
        refused(&device, "guest links are off by policy");
        let msg = "Guest links are turned off on this computer by its organization.".into();
        control::write_json(&mut send, &ServerMsg::Denied { msg }).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"links off");
        return Ok(());
    }
    if auth.is_none()
        && let Some(p) = &pass
        && let Some(valid) = crate::access::check_pass(&access_file, p)
    {
        auth = Some(Auth::Guest {
            pass: p.clone(),
            expires: valid.expires,
            new: false,
            scope: valid.scope,
            passkey: valid.passkey,
        });
    }
    let mut link_failed = false;
    if auth.is_none()
        && let Some(code) = &link
    {
        match crate::access::redeem_link(&access_file, code, &device, peer) {
            Ok(Some(g)) => {
                auth = Some(Auth::Guest {
                    pass: g.pass,
                    expires: g.expires,
                    new: true,
                    scope: g.scope,
                    passkey: None,
                })
            }
            Ok(None) => link_failed = true,
            Err(e) => warn!("{peer}: magic link check failed: {e:#}"),
        }
    }
    let Some(auth) = auth else {
        record_auth_failure(shared, peer);
        warn!("{peer}: rejected (bad or missing token)");
        refused(&device, if link_failed { "used or expired link" } else { "no valid token" });
        if link_failed {
            let msg = "This link has expired or was already used. Ask for a new one.".into();
            control::write_json(&mut send, &ServerMsg::Error { msg }).await?;
        }
        control::write_json(
            &mut send,
            &ServerMsg::Error {
                msg: "unauthorized".into(),
            },
        )
        .await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"unauthorized");
        return Ok(());
    };
    if let Auth::Guest {
        pass,
        expires,
        new: true,
        ..
    } = &auth
    {
        // Sent first, so the device keeps its pass even if a later step needs a retry.
        control::write_json(
            &mut send,
            &ServerMsg::Pass {
                pass: pass.clone(),
                expires: *expires,
            },
        )
        .await?;
    }
    // A link made with --passkey: the pass only works together with its passkey.
    if let Auth::Guest {
        pass,
        scope,
        passkey,
        ..
    } = &auth
        && scope.passkey
    {
        let ok = match passkey {
            Some(key) => {
                crate::passkey::verify(&mut send, &mut recv, &access_file, pass, key).await
            }
            None => {
                crate::passkey::register(&mut send, &mut recv, &access_file, pass, &device).await
            }
        };
        if !matches!(ok, Ok(true)) {
            if let Err(e) = &ok {
                warn!("{peer}: passkey: {e:#}");
            }
            record_auth_failure(shared, peer);
            refused(&device, "passkey check failed");
            // Denied, not an error: retrying on its own would only prompt again (and again).
            let msg = "This link needs a passkey, and it didn't check out. Try again, or ask for a new link.".into();
            control::write_json(&mut send, &ServerMsg::Denied { msg }).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.conn.close(1u32.into(), b"passkey required");
            return Ok(());
        }
    }
    let mut perms = match &auth {
        Auth::Guest { scope, .. } | Auth::TailnetGuest { scope, .. } => control::Perms::guest(scope),
        _ => control::Perms::owner(),
    };
    // Files and the clipboard can be off for everyone, by policy.
    perms.files &= policy.allows("files");
    perms.clipboard &= policy.allows("clipboard");
    let terminal = crate::terminal::SUPPORTED && policy.allows("terminal");
    // The sign-in host runs as SYSTEM: its owner-only features would be the whole computer.
    // Everyone gets the screen and input there, and nothing else.
    let signin = crate::signin::active();
    let asked = !perms.owner;
    if signin {
        perms.owner = false;
        perms.files = false;
        perms.clipboard = false;
    }
    let terminal = terminal && !signin;
    let guest = !perms.owner;
    if matches!(auth, Auth::Tailnet | Auth::TailnetGuest { .. })
        && let Some(name) = tailscale::device_name(peer).await
    {
        device = format!("{name} ({device})");
    }
    let how = match &auth {
        Auth::Token => crate::viewers::How::Token,
        Auth::Tailnet | Auth::TailnetGuest { .. } => crate::viewers::How::Tailnet,
        Auth::Guest { .. } => crate::viewers::How::Guest,
    };
    let session_id = crate::viewers::next_id();
    let viewer = crate::viewers::Viewer {
        id: session_id,
        device: device.clone(),
        how,
        remote,
        peer,
        since: std::time::SystemTime::now(),
    };
    let scope_detail = match &auth {
        Auth::Guest { scope, .. } => scope.describe(),
        Auth::TailnetGuest { login, scope } => format!("{login}, {}", scope.describe()),
        _ => String::new(),
    };

    // Device posture: what this computer requires of devices, checked before asking anyone.
    if let Err(why) = crate::posture::check(peer, ctx.relayed.is_some(), ua.as_deref()).await {
        warn!("{peer}: refused by device posture: {why}");
        refused(&device, &format!("posture: {why}"));
        let msg = format!("Not allowed: {why}.");
        control::write_json(&mut send, &ServerMsg::Denied { msg }).await?;
        tokio::time::sleep(Duration::from_millis(200)).await;
        ctx.conn.close(1u32.into(), b"device posture");
        return Ok(());
    }

    // Ask before connecting: the person at the computer allows (or denies) this viewer.
    let settings = crate::settings::load();
    let scope_asks = matches!(&auth, Auth::Guest { scope, .. } | Auth::TailnetGuest { scope, .. } if scope.ask);
    // (Nobody is at the sign-in screen to ask.)
    if !signin && (settings.ask_everyone || (asked && (settings.ask_guests || scope_asks))) {
        let msg = "Waiting for the person at this computer to let you in…".into();
        control::write_json(&mut send, &ServerMsg::Waiting { msg }).await?;
        let mut who = viewer.describe();
        if let Some(cut) = who.rfind(" · ") {
            who.truncate(cut); // drop "· just now"
        }
        if !crate::approval::ask(who).await {
            refused(&device, "not allowed");
            let msg = "The person at this computer didn't let you in.".into();
            control::write_json(&mut send, &ServerMsg::Denied { msg }).await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            ctx.conn.close(1u32.into(), b"not allowed");
            return Ok(());
        }
    }

    let _ = ctx.perms.set(perms);
    ctx.authed.store(true, Ordering::Relaxed);
    if let Some(r) = &ctx.relayed {
        crate::remote::bridge_authed(r.bridge);
    }
    shared.video.request_keyframe(None);
    match &auth {
        Auth::Token => info!("{peer}: authenticated via token"),
        Auth::Tailnet => info!("{peer}: authenticated via tailnet identity"),
        Auth::TailnetGuest { login, scope } => {
            info!("{peer}: authenticated via tailnet identity {login}, a guest ({})", scope.describe())
        }
        Auth::Guest {
            expires,
            new,
            scope,
            ..
        } => {
            let left = crate::access::describe(Duration::from_secs(
                expires.saturating_sub(crate::access::now()),
            ));
            let how = if *new { "magic link" } else { "guest pass" };
            info!("{peer}: authenticated via {how} ({left} left, {})", scope.describe());
        }
    }
    let _log = crate::connlog::Session {
        entry: crate::connlog::Entry {
            start: crate::access::now(),
            secs: 0,
            device: device.clone(),
            how: match how {
                crate::viewers::How::Token => "token",
                crate::viewers::How::Tailnet => "tailnet",
                crate::viewers::How::Guest => "guest",
            }
            .into(),
            remote,
            peer: peer.to_string(),
            detail: scope_detail,
        },
        started: std::time::Instant::now(),
    };
    // This viewer's resolution request lasts as long as the session.
    let viewport_id = session_id;
    struct ViewportGuard(u64, Arc<Shared>);
    impl Drop for ViewportGuard {
        fn drop(&mut self) {
            crate::resolution::set(self.0, None);
            crate::refresh::set(self.0, None);
            end_fit(self.0, self.1.clone());
            end_extend(self.0, self.1.clone());
        }
    }
    let _viewport = ViewportGuard(viewport_id, ctx.shared.clone());
    // 60 fps until the viewer says its screen is faster.
    crate::refresh::set(viewport_id, Some(crate::refresh::DEFAULT));
    let _codec = crate::codec::joined(
        session_id,
        codecs.iter().any(|c| c == "hevc"),
        ctx.relayed.as_ref().is_some_and(|r| r.rtp_video),
    );
    let _sharp = crate::sharp::joined(session_id, codecs.iter().any(|c| c == "hevc-rext"));
    let mut sharp_on = false;
    let thumbs_busy = Arc::new(AtomicBool::new(false));
    let thumbs_sent = Arc::new(std::sync::Mutex::new(crate::thumbs::Sent::default()));
    // Sharp text's lossless pictures of a still screen; <video> (RTP) viewers can't show them.
    let rtp_viewer = ctx.relayed.as_ref().is_some_and(|r| r.rtp_video);
    let lossless_reset = Arc::new(AtomicBool::new(false));
    if shared.video.supports_lossless() && !rtp_viewer {
        let (conn, shared, reset) = (ctx.conn.clone(), shared.clone(), lossless_reset.clone());
        tokio::spawn(crate::lossless::run(
            conn,
            shared,
            move || crate::sharp::session_on(session_id),
            reset,
        ));
    }
    // Shown on the host (menu bar / notification) for as long as this session lasts.
    let _viewing = crate::viewers::joined(viewer, ctx.conn.clone());
    // Recording every session (glass config record on), or when an owner asks.
    let mut recording = if settings.record {
        match crate::recording::start(&device) {
            Ok(r) => Some(r),
            Err(e) => {
                warn!("{peer}: recording: {e:#}");
                None
            }
        }
    } else {
        None
    };
    if let Auth::Guest { pass, .. } = &auth {
        // A guest is cut off when its pass expires or is revoked (`glass link --revoke`).
        let (conn, pass, file) = (ctx.conn.clone(), pass.clone(), access_file.clone());
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = conn.closed() => return,
                    _ = tokio::time::sleep(GUEST_RECHECK) => {}
                }
                if crate::access::pass_expiry(&file, &pass).is_none() {
                    info!("{}: guest access ended", conn.remote_address());
                    conn.close(1u32.into(), b"access ended");
                    return;
                }
            }
        });
    }

    let displays = {
        let shared = shared.clone();
        tokio::task::spawn_blocking(move || shared.video.displays())
            .await?
            .unwrap_or_default()
    };
    let features = control::Features {
        privacy: crate::privacy::supported(),
        gamepad: crate::gamepad::supported(),
        mic: crate::mic::supported(),
        mic_install: perms.owner && !crate::mic::supported(),
        windows: shared.video.supports_windows(),
        terminal,
        restart: true,
        record: crate::recording::supported(),
        hevc: shared.video.supports_hevc(),
        // Forwarded ports are plain HTTP on the tailnet; there's no way to them over WebRTC.
        forward_ports: if perms.owner && ctx.relayed.is_none() {
            settings.forward_ports.clone()
        } else {
            Vec::new()
        },
        lossless: shared.video.supports_lossless()
            && !ctx.relayed.as_ref().is_some_and(|r| r.rtp_video),
        push: crate::push::public_key(),
        browse: perms.owner && perms.files,
        extend: cfg!(target_os = "macos") || crate::testmode::enabled(),
        fit: cfg!(target_os = "macos") || crate::testmode::enabled(),
        support: crate::support::is_host(),
    };
    // Someone other than the owner is in: worth a notification on the owner's devices.
    if !perms.owner {
        let mut who = crate::viewers::subscribe().borrow().iter().find(|v| v.id == session_id).map(|v| v.describe()).unwrap_or_else(|| device.clone());
        if let Some(cut) = who.rfind(" · ") {
            who.truncate(cut);
        }
        crate::push::activity("Someone joined", &who, &format!("joined-{session_id}"));
    }
    let welcome = ServerMsg::Welcome {
        displays,
        streams: shared.video.streams(),
        audio: crate::audio::SUPPORTED,
        host_os: if crate::testmode::enabled() {
            crate::testmode::host_os()
        } else {
            crate::platform::OS
        },
        guest,
        session: session_id,
        perms,
        features,
        name: settings.name.clone(),
        accent: settings.accent.clone(),
        computer: computer_name().to_owned(),
        codec: crate::codec::current(),
    };
    control::write_json(&mut send, &welcome).await?;
    let bps = shared.rate.lock().unwrap().target();
    control::write_json(&mut send, &ServerMsg::Bitrate { bps }).await?;
    if crate::privacy::active() {
        control::write_json(&mut send, &ServerMsg::Privacy { on: true }).await?;
    }
    if let Some(r) = &recording {
        let path = Some(r.path.display().to_string());
        control::write_json(&mut send, &ServerMsg::Recording { on: true, path }).await?;
    }
    if let Some(info) = crate::now_playing::current() {
        control::write_json(&mut send, &ServerMsg::NowPlaying { info }).await?;
    }
    for msg in crate::computer::snapshot() {
        control::write_json(&mut send, &msg).await?;
    }
    if perms.owner
        && let Some(version) = crate::update::available()
    {
        control::write_json(
            &mut send,
            &ServerMsg::UpdateAvailable {
                current: crate::update::CURRENT,
                version,
            },
        )
        .await?;
    }

    let mut events = shared.events.subscribe();
    tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                msg = ctl_rx.recv() => match msg { Some(m) => m, None => break },
                msg = events.recv() => match msg {
                    Ok(m) => m,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            };
            // A link without clipboard access never sees the host's clipboard.
            if matches!(msg, ServerMsg::Clipboard { .. }) && !perms.clipboard {
                continue;
            }
            if control::write_json(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    let mut term = crate::terminal::Session::default();
    let refuse = |msg: &str| {
        let _ = ctx.ctl_tx.send(ServerMsg::Error { msg: msg.into() });
    };
    const OWNER_ONLY: &str = "Only this computer's owner can do that.";
    while let Some(buf) = control::read_frame(&mut recv).await? {
        // Skip what we don't understand (say, a newer client) rather than dropping the
        // whole control stream, which would silently break display switching and more.
        let msg = match serde_json::from_slice::<ClientMsg>(&buf) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("{peer}: ignoring control message: {e}");
                continue;
            }
        };
        match msg {
            ClientMsg::Hello { .. } | ClientMsg::Passkey(_) => {}
            ClientMsg::SelectDisplay { id } => set_layout(&ctx, Layout::Single(Some(id))).await?,
            ClientMsg::ShowAll => set_layout(&ctx, Layout::All).await?,
            ClientMsg::Clipboard { .. } if !perms.clipboard => {
                warn!("{peer}: clipboard is off for this link; ignored")
            }
            ClientMsg::ClipboardImage if !perms.clipboard => {}
            ClientMsg::Clipboard { text, html, paste } => {
                match crate::clip::html_for_viewer(html) {
                    Some(html) => shared.clipboard.set(&crate::clip::Clip { text, html: Some(html), image: None }),
                    None => shared.clipboard.set_text(&text),
                }
                if paste && perms.input {
                    shared.injector.lock().unwrap().paste();
                }
            }
            ClientMsg::ClipboardImage => {
                let (clipboard, conn) = (shared.clipboard.clone(), ctx.conn.clone());
                let ctl = ctx.ctl_tx.clone();
                tokio::spawn(async move {
                    let png = tokio::task::spawn_blocking(move || clipboard.image_png()).await.ok().flatten();
                    let result = match png {
                        Some(png) if png.len() <= crate::clip::MAX_IMAGE => crate::clip::send_image(&conn, &png).await,
                        Some(_) => Err(anyhow::anyhow!("The image on the clipboard is too big to send (over 8 MB).")),
                        None => Err(anyhow::anyhow!("There's no image on the clipboard any more.")),
                    };
                    if let Err(e) = result {
                        let _ = ctl.send(ServerMsg::Error { msg: format!("{e:#}") });
                    }
                });
            }
            ClientMsg::Video { on } => {
                ctx.video_on.store(on, Ordering::Relaxed);
                if let Some(r) = &ctx.relayed {
                    crate::remote::bridge_watching(r.bridge, on);
                }
                if on {
                    shared.video.request_keyframe(None);
                }
                info!("{peer}: video {}", if on { "resumed" } else { "paused" });
            }
            ClientMsg::Update if guest => warn!("{peer}: guest asked to update the host; ignored"),
            ClientMsg::Update => {
                if crate::testmode::enabled() {
                    crate::testmode::record_update();
                } else if let Err(e) = crate::update::start_self_update() {
                    let _ = ctx.ctl_tx.send(ServerMsg::Error {
                        msg: format!("update: {e:#}"),
                    });
                }
            }
            ClientMsg::PushSubscribe { subscription } => {
                let (device, owner) = (device.clone(), perms.owner);
                let reply = match tokio::task::spawn_blocking(move || crate::push::subscribe(subscription, &device, owner)).await? {
                    Ok(key) => {
                        info!("{peer}: notifications on");
                        ServerMsg::Push { key: Some(key), owner }
                    }
                    Err(e) => {
                        refuse(&format!("Couldn't turn on notifications: {e:#}"));
                        ServerMsg::Push { key: None, owner }
                    }
                };
                let _ = ctx.ctl_tx.send(reply);
            }
            ClientMsg::PushUnsubscribe { endpoint } => {
                let _ = tokio::task::spawn_blocking(move || crate::push::remove(&endpoint)).await?;
                info!("{peer}: notifications off");
            }
            ClientMsg::PushSeen { endpoint } => {
                tokio::task::spawn_blocking(move || crate::push::seen(&endpoint));
            }
            ClientMsg::Refresh { hz } => {
                let fps = crate::refresh::snap(hz);
                info!("{peer}: screen refreshes at {hz:.0} Hz; asks for {fps} fps");
                crate::refresh::set(viewport_id, Some(fps));
            }
            ClientMsg::Fit { .. } if guest => {
                refuse("Only this computer's owner can change its display shape.")
            }
            ClientMsg::Fit { size } => fit(&ctx, viewport_id, size).await?,
            ClientMsg::Extend { .. } if guest => refuse("Only this computer's owner can add a display."),
            ClientMsg::Extend { size, hidpi } => extend(&ctx, viewport_id, size, hidpi).await?,
            // Sharp text keeps this viewer at native size, whatever its screen.
            ClientMsg::Viewport { .. } if sharp_on => {}
            ClientMsg::LosslessReset => lossless_reset.store(true, Ordering::Relaxed),
            ClientMsg::Sharp { .. } if !perms.input => refuse("This link is view only."),
            ClientMsg::Sharp { on } => {
                sharp_on = on;
                if on {
                    // An empty request: every stream at full resolution.
                    crate::resolution::set(viewport_id, Some(Vec::new()));
                }
                // Turning it off: the viewer sends its viewport again.
                crate::sharp::set(session_id, on);
                info!("{peer}: sharp text {}", if on { "on" } else { "off" });
            }
            ClientMsg::Viewport { streams } => {
                let streams = streams
                    .into_iter()
                    .take(MAX_STREAMS)
                    .map(|s| s.map(|[w, h]| [w.min(16_384), h.min(16_384)]))
                    .collect();
                crate::resolution::set(viewport_id, Some(streams));
            }
            ClientMsg::Log { lines } => {
                if let Some(path) = CLIENT_LOG.get() {
                    let body = lines
                        .iter()
                        .take(LOG_LINES_MAX)
                        .map(|l| {
                            l.chars()
                                .take(LOG_LINE_MAX)
                                .collect::<String>()
                                .replace('\n', " ")
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    append_client_log(path.clone(), body).await;
                }
            }
            ClientMsg::NetStats(report) => {
                apply_rate_report(shared, report);
            }
            ClientMsg::Audio { on } => {
                ctx.audio_on.store(on, Ordering::Relaxed);
                info!("{peer}: audio {}", if on { "on" } else { "off" });
                #[cfg(windows)]
                if on && crate::platform::audio::unavailable().is_some() {
                    let _ = ctx.ctl_tx.send(ServerMsg::Notice {
                        msg: "No sound to share: this computer has no sound output right now.".into(),
                    });
                }
            }
            ClientMsg::FilesList { .. }
            | ClientMsg::FilesFetch { .. }
            | ClientMsg::FilesMkdir { .. }
            | ClientMsg::FilesRename { .. }
            | ClientMsg::FilesTrash { .. }
            | ClientMsg::FilesThumb { .. }
                if !(perms.owner && perms.files) =>
            {
                refuse("The file browser is for the computer's owner.")
            }
            ClientMsg::FilesList { path, hidden } => {
                let tx = ctx.ctl_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(files_listing(&path, hidden));
                });
            }
            ClientMsg::FilesFetch { path } => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let result = match crate::browse::resolve(&crate::browse::root(), &path) {
                        Ok(p) if p.is_dir() => Err(anyhow::anyhow!("folders can't be downloaded; open it and pick files")),
                        Ok(p) => files::send(&ctx.conn, &p).await,
                        Err(e) => Err(e),
                    };
                    if let Err(e) = result {
                        let _ = ctx.ctl_tx.send(ServerMsg::Error { msg: format!("{e:#}") });
                    }
                });
            }
            ClientMsg::FilesMkdir { path, name } => {
                let tx = ctx.ctl_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let r = crate::browse::mkdir(&crate::browse::root(), &path, &name).map(|_| ());
                    files_changed(&tx, &path, r);
                });
            }
            ClientMsg::FilesRename { path, name } => {
                let tx = ctx.ctl_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let parent = parent_of(&path);
                    let r = crate::browse::rename(&crate::browse::root(), &path, &name).map(|_| ());
                    files_changed(&tx, &parent, r);
                });
            }
            ClientMsg::FilesTrash { path } => {
                let tx = ctx.ctl_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let parent = parent_of(&path);
                    let r = crate::browse::trash(&crate::browse::root(), &path);
                    if r.is_ok() {
                        info!("file browser: moved {path} to the Trash");
                    }
                    files_changed(&tx, &parent, r);
                });
            }
            ClientMsg::FilesThumb { path } => {
                let tx = ctx.ctl_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let data = crate::browse::thumbnail(&crate::browse::root(), &path).ok().flatten();
                    let _ = tx.send(ServerMsg::FilesThumb { path, data });
                });
            }
            ClientMsg::PickFiles if !perms.files => refuse("Files are turned off for this link."),
            ClientMsg::PickFiles => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    match files::pick().await {
                        Ok(paths) => {
                            for path in paths {
                                if let Err(e) = files::send(&ctx.conn, &path).await {
                                    let _ = ctx.ctl_tx.send(ServerMsg::Error {
                                        msg: format!("send {}: {e:#}", path.display()),
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            let _ = ctx.ctl_tx.send(ServerMsg::Error {
                                msg: format!("pick files: {e:#}"),
                            });
                        }
                    }
                });
            }
            ClientMsg::Chat { text } => {
                let text: String = text.trim().chars().take(CHAT_MAX).collect();
                if text.is_empty() {
                    continue;
                }
                info!("{peer}: chat ({} chars)", text.chars().count());
                crate::notify::notify(&format!("Message from {device}"), &text);
                remember_chat(&device, false, &text);
                crate::push::chat(&device, &text);
                let _ = shared.events.send(ServerMsg::Chat {
                    from: device.clone(),
                    sender: session_id,
                    text,
                });
            }
            ClientMsg::Restart { .. } if guest => refuse(OWNER_ONLY),
            ClientMsg::Restart { glass } => {
                let result = if glass {
                    crate::power::restart_glass()
                } else {
                    crate::power::restart_computer()
                };
                if let Err(e) = result {
                    refuse(&format!("Couldn't restart: {e:#}"));
                }
            }
            ClientMsg::Wake { .. } if guest => refuse(OWNER_ONLY),
            ClientMsg::Wake { dns } => match crate::wol::wake_target(&dns) {
                Ok(_) => {
                    let _ = ctx.ctl_tx.send(ServerMsg::Notice {
                        msg: format!("Sent a wake-up to {dns}. It can take a minute to come online."),
                    });
                }
                Err(e) => refuse(&format!("Couldn't wake {dns}: {e:#}")),
            },
            ClientMsg::Sas if !perms.input => refuse("This link is view only."),
            ClientMsg::Sas => {
                #[cfg(windows)]
                if !crate::platform::secure::send_sas() {
                    refuse("Ctrl+Alt+Del needs the Glass secure-desktop service. Install it with `glass service install`.");
                }
                #[cfg(not(windows))]
                refuse("Ctrl+Alt+Del is only available on Windows.");
            }
            ClientMsg::Agent { .. } | ClientMsg::AgentAnswer { .. } if guest => refuse(OWNER_ONLY),
            ClientMsg::Agent { op } => crate::computer::control(&op),
            ClientMsg::AgentAnswer { id, ok, note } => {
                // A viewer on this computer itself: an agent on the screen could be clicking it.
                let local = crate::oauth::is_this_computer(ctx.conn.remote_address().ip());
                crate::computer::answer_from(id, crate::computer::Answer { ok, note }, local)
            }
            ClientMsg::Privacy { .. } if guest => refuse(OWNER_ONLY),
            ClientMsg::Privacy { on } => {
                if let Err(e) = crate::privacy::set_forced(on) {
                    refuse(&format!("{e:#}"));
                }
            }
            ClientMsg::Thumbs => {
                // One at a time per viewer; a request while one is running is dropped.
                if !thumbs_busy.swap(true, Ordering::AcqRel) {
                    let shared = shared.clone();
                    let tx = ctx.ctl_tx.clone();
                    let busy = thumbs_busy.clone();
                    let sent = thumbs_sent.clone();
                    tokio::spawn(async move {
                        let pictures = tokio::task::spawn_blocking(move || {
                            let displays = shared.video.displays().unwrap_or_default();
                            displays
                                .iter()
                                .filter_map(|d| {
                                    let frame = shared.video.picture(d.id, crate::thumbs::MAX)?;
                                    Some((d.id, crate::thumbs::png(&frame).ok()?))
                                })
                                .collect::<Vec<_>>()
                        })
                        .await
                        .unwrap_or_default();
                        let thumbs = sent.lock().unwrap().changed(pictures);
                        let _ = tx.send(ServerMsg::Thumbs { thumbs });
                        busy.store(false, Ordering::Release);
                    });
                }
            }
            ClientMsg::ListWindows => {
                let shared = shared.clone();
                let tx = ctx.ctl_tx.clone();
                tokio::spawn(async move {
                    let windows = tokio::task::spawn_blocking(move || shared.video.windows())
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                    let _ = tx.send(ServerMsg::Windows { windows });
                });
            }
            ClientMsg::SelectWindow { id } => set_layout(&ctx, Layout::Window(id)).await?,
            ClientMsg::Annotate { stroke } => {
                if stroke.to_string().len() <= ANNOTATION_MAX {
                    let _ = shared.events.send(ServerMsg::Annotate {
                        sender: session_id,
                        stroke,
                    });
                }
            }
            ClientMsg::AnnotateClear => {
                let _ = shared
                    .events
                    .send(ServerMsg::AnnotateClear { sender: session_id });
            }
            ClientMsg::Media { .. } if !perms.input => refuse("This link is view only."),
            ClientMsg::Media { action, position } => crate::now_playing::command(&action, position),
            ClientMsg::MicInstall if guest => refuse(OWNER_ONLY),
            ClientMsg::MicInstall => {
                let tx = ctx.ctl_tx.clone();
                tokio::spawn(async move {
                    // The password dialog shows on this computer's screen (seen in the stream).
                    let msg = match tokio::task::spawn_blocking(|| crate::mic::install(crate::mic::Ask::Dialog)).await {
                        Ok(Ok(m)) => ServerMsg::Notice { msg: m },
                        Ok(Err(e)) => ServerMsg::Error { msg: format!("Couldn't set up the microphone: {e:#}") },
                        Err(_) => return,
                    };
                    let _ = tx.send(msg);
                    // Tell this viewer as soon as it works (an installer can take a while).
                    for _ in 0..300 {
                        if tokio::task::spawn_blocking(crate::mic::supported).await.unwrap_or(false) {
                            let _ = tx.send(ServerMsg::MicReady);
                            return;
                        }
                        if tx.is_closed() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                });
            }
            ClientMsg::Mic { .. } if !perms.input => refuse("This link is view only."),
            ClientMsg::Mic { on } => {
                ctx.mic_on.store(on, Ordering::Relaxed);
                crate::mic::active(on);
                info!("{peer}: microphone {}", if on { "on" } else { "off" });
            }
            ClientMsg::Term(_) if guest => refuse(OWNER_ONLY),
            ClientMsg::Term(_) if !terminal => refuse("The terminal is turned off on this computer by its organization."),
            ClientMsg::Term(msg) => {
                if let Err(e) = term.handle(msg, ctx.ctl_tx.clone()) {
                    refuse(&format!("{e:#}"));
                }
            }
            ClientMsg::Record { .. } if guest => refuse(OWNER_ONLY),
            ClientMsg::Record { on } => {
                if on && recording.is_none() {
                    match crate::recording::start(&device) {
                        Ok(r) => {
                            let path = Some(r.path.display().to_string());
                            recording = Some(r);
                            let _ = ctx.ctl_tx.send(ServerMsg::Recording { on: true, path });
                        }
                        Err(e) => refuse(&format!("Couldn't record: {e:#}")),
                    }
                } else if !on && let Some(r) = recording.take() {
                    let path = Some(r.path.display().to_string());
                    drop(r);
                    let _ = ctx.ctl_tx.send(ServerMsg::Recording { on: false, path });
                }
            }
        }
    }
    if ctx.mic_on.load(Ordering::Relaxed) {
        crate::mic::active(false);
    }
    Ok(())
}

/// Chat messages and drawings are capped (characters / serialized bytes).
const CHAT_MAX: usize = 2000;
const ANNOTATION_MAX: usize = 64 << 10;

/// A message from the person at the computer (`glass chat`, the menu bar) to every viewer;
/// returns how many sessions are listening.
pub fn host_chat(text: &str) -> Result<usize> {
    let shared = shared().ok_or_else(|| anyhow::anyhow!("the host isn't ready yet"))?;
    let text: String = text.trim().chars().take(CHAT_MAX).collect();
    anyhow::ensure!(!text.is_empty(), "nothing to send");
    let from = crate::settings::load()
        .name
        .unwrap_or_else(|| "This computer".into());
    let n = crate::viewers::subscribe().borrow().len();
    anyhow::ensure!(n > 0, "nobody is connected");
    remember_chat(&from, true, &text);
    crate::push::chat(&from, &text);
    let _ = shared.events.send(ServerMsg::Chat {
        from,
        sender: 0,
        text,
    });
    Ok(n)
}

/// A folder for the file browser, or why it can't be shown.
fn files_listing(path: &str, hidden: bool) -> ServerMsg {
    match crate::browse::list(&crate::browse::root(), path, hidden) {
        Ok(l) => ServerMsg::Files { path: l.path, entries: l.entries, truncated: l.truncated, ticket: crate::browse::ticket(), error: None },
        Err(e) => ServerMsg::Files { path: path.into(), entries: Vec::new(), truncated: false, ticket: String::new(), error: Some(format!("{e:#}")) },
    }
}

/// After a change in the file browser: the folder again, or the error.
fn files_changed(tx: &mpsc::UnboundedSender<ServerMsg>, folder: &str, result: Result<()>) {
    if let Err(e) = result {
        let _ = tx.send(ServerMsg::Error { msg: format!("{e:#}") });
    }
    let _ = tx.send(files_listing(folder, false));
}

fn parent_of(path: &str) -> String {
    path.trim_matches('/').rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default()
}

/// One chat message, kept (in memory only) so the app can show the conversation.
#[derive(Clone, serde::Serialize)]
pub struct ChatLine {
    pub at: u64,
    pub from: String,
    /// Sent from this computer, not by a viewer.
    pub mine: bool,
    pub text: String,
}

const CHAT_KEEP: usize = 200;
static CHAT_LOG: Mutex<std::collections::VecDeque<ChatLine>> = Mutex::new(std::collections::VecDeque::new());

fn remember_chat(from: &str, mine: bool, text: &str) {
    let mut log = CHAT_LOG.lock().unwrap();
    if log.len() == CHAT_KEEP {
        log.pop_front();
    }
    log.push_back(ChatLine { at: crate::access::now(), from: from.into(), mine, text: text.into() });
}

/// The conversation so far, oldest first.
pub fn chat_log() -> Vec<ChatLine> {
    CHAT_LOG.lock().unwrap().iter().cloned().collect()
}

/// "Fit this device" (macOS): a virtual display in one viewer's shape, which the main
/// display mirrors while it lasts. One at a time; it ends with that viewer's session.
#[cfg(target_os = "macos")]
struct Fit {
    owner: u64,
    /// Held, not read: dropping it unmirrors and removes the display.
    _display: crate::platform::virtual_display::VirtualDisplay,
}

#[cfg(target_os = "macos")]
static FIT: Mutex<Option<Fit>> = Mutex::new(None);

#[cfg(target_os = "macos")]
async fn fit(ctx: &SessionCtx, session: u64, size: Option<[u32; 2]>) -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_fit(size); // never reshape real displays in tests
        return Ok(());
    }
    let peer = ctx.conn.remote_address();
    // Put any current one back first (dropping it unmirrors and removes it).
    let old = FIT.lock().unwrap().take();
    // The display to reshape: the one this fit replaces mirrored, else the one being
    // watched (main when it's a window, every display, or a second display we added).
    let target = match old.as_ref().and_then(|f| f._display.replaced()) {
        Some(id) => Some(id),
        None => {
            let shared = ctx.shared.clone();
            tokio::task::spawn_blocking(move || {
                let streams = shared.video.streams();
                let [watching] = streams.as_slice() else { return None };
                let added = EXTENDED.lock().unwrap().iter().any(|(_, d)| d.id == watching.id);
                let physical = shared.video.displays().is_ok_and(|ds| ds.iter().any(|d| d.id == watching.id));
                (physical && !added).then_some(watching.id)
            })
            .await?
        }
    };
    let size = size.map(|[w, h]| [w & !3, h & !3]); // even points
    // A new size for the same viewer (it turned): resize the display it has, so the windows
    // on it stay there.
    let old = match (old, size) {
        (Some(mut fit), Some([w, h])) => {
            let (fit, resized) = tokio::task::spawn_blocking(move || {
                let resized = fit._display.resize(w, h);
                (fit, resized)
            })
            .await?;
            match resized {
                Ok(()) => {
                    let id = fit._display.id;
                    info!("{peer}: fit resized to {w}×{h} (virtual display {id})");
                    *FIT.lock().unwrap() = Some(Fit { owner: session, _display: fit._display });
                    fit_windows(id);
                    return set_layout(ctx, Layout::Single(Some(id))).await;
                }
                Err(e) => {
                    info!("{peer}: can't resize the fit in place ({e:#}); making a new one");
                    Some(fit)
                }
            }
        }
        (old, _) => old,
    };
    let had = old.is_some();
    if let Some(old) = old {
        tokio::task::spawn_blocking(move || drop(old)).await?;
    }
    let Some([w, h]) = size else {
        if had {
            info!("{peer}: displays restored");
            set_layout(ctx, Layout::Single(target)).await?;
        }
        return Ok(());
    };
    let created = tokio::task::spawn_blocking(move || {
        // Retina: the viewer's pixels, at the size things look on it.
        let mut display = crate::platform::virtual_display::VirtualDisplay::create_scaled(w, h, true)?;
        display.replace(target.unwrap_or_else(|| objc2_core_graphics::CGMainDisplayID()))?;
        anyhow::Ok(display)
    })
    .await?;
    let display = match created {
        Ok(d) => d,
        Err(e) => {
            warn!("{peer}: fit {w}×{h} failed: {e:#}");
            let msg = format!("Couldn't fit this device: {e:#}");
            let _ = ctx.ctl_tx.send(ServerMsg::Error { msg });
            return Ok(());
        }
    };
    let id = display.id;
    let [vw, vh] = [display.width, display.height];
    info!("{peer}: fitting the desktop to {vw}×{vh} for {w}×{h} (virtual display {id})");
    *FIT.lock().unwrap() = Some(Fit {
        owner: session,
        _display: display,
    });
    // Screen capture sees a new display a moment after it appears.
    for _ in 0..30 {
        let shared = ctx.shared.clone();
        let listed = tokio::task::spawn_blocking(move || shared.video.displays())
            .await?
            .is_ok_and(|ds| ds.iter().any(|d| d.id == id));
        if listed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    fit_windows(id);
    set_layout(ctx, Layout::Single(Some(id))).await
}

/// Once a fitted display has its size: windows that don't fit on it any more shrink and
/// move onto it (in the background; asking every app can take a moment).
#[cfg(target_os = "macos")]
fn fit_windows(id: u32) {
    tokio::task::spawn_blocking(move || {
        // macOS moves windows off a display that changed a moment after it does.
        std::thread::sleep(Duration::from_millis(400));
        let b = objc2_core_graphics::CGDisplayBounds(id);
        let display = [b.origin.x, b.origin.y, b.size.width, b.size.height];
        // The menu bar is on the main display.
        let bar = if objc2_core_graphics::CGMainDisplayID() == id { MENU_BAR } else { 0.0 };
        let area = [display[0], display[1] + bar, display[2], display[3] - bar];
        let moved = crate::platform::ax::fit_windows(display, area);
        if moved > 0 {
            let [_, _, fw, fh] = area;
            info!("fit: {moved} windows resized or moved to fit {fw}×{fh}");
        }
    });
}

/// The menu bar's height, in points.
#[cfg(target_os = "macos")]
const MENU_BAR: f64 = 25.0;

#[cfg(not(target_os = "macos"))]
async fn fit(ctx: &SessionCtx, _session: u64, size: Option<[u32; 2]>) -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_fit(size);
        return Ok(());
    }
    if size.is_some() {
        let msg = "Fitting the desktop to this device is only available on a Mac for now.".into();
        let _ = ctx.ctl_tx.send(ServerMsg::Error { msg });
    }
    Ok(())
}

/// "Use as a second display" (macOS): a virtual display beside the main one, extending the
/// desktop onto a viewer's screen. One per viewer; it ends with that viewer's session.
#[cfg(target_os = "macos")]
static EXTENDED: Mutex<Vec<(u64, crate::platform::virtual_display::VirtualDisplay)>> = Mutex::new(Vec::new());

async fn extend(ctx: &SessionCtx, session: u64, size: Option<[u32; 2]>, hidpi: bool) -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record("extend", serde_json::json!({ "size": size, "hidpi": hidpi }));
        return Ok(());
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = session;
        if size.is_some() {
            let msg = "Using this device as a second display isn't available on Linux yet.".into();
            let _ = ctx.ctl_tx.send(ServerMsg::Error { msg });
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let peer = ctx.conn.remote_address();
        // This viewer's earlier one goes first (dropping it removes the display).
        let old: Vec<_> = {
            let mut list = EXTENDED.lock().unwrap();
            let (mine, rest): (Vec<_>, Vec<_>) = list.drain(..).partition(|(s, _)| *s == session);
            *list = rest;
            mine
        };
        let had = !old.is_empty();
        if had {
            tokio::task::spawn_blocking(move || drop(old)).await?;
        }
        let Some([w, h]) = size else {
            if had {
                info!("{peer}: second display removed");
                set_layout(ctx, Layout::Single(None)).await?;
            }
            return Ok(());
        };
        // Even sizes (and even points, for Retina).
        let (w, h) = if hidpi { (w & !3, h & !3) } else { (w & !1, h & !1) };
        let created = tokio::task::spawn_blocking(move || {
            let display = crate::platform::virtual_display::VirtualDisplay::create_scaled(w, h, hidpi)?;
            display.place_right_of_main()?;
            anyhow::Ok(display)
        })
        .await?;
        let display = match created {
            Ok(d) => d,
            Err(e) => {
                warn!("{peer}: second display {w}×{h} failed: {e:#}");
                let msg = format!("Couldn't add a display: {e:#}");
                let _ = ctx.ctl_tx.send(ServerMsg::Error { msg });
                return Ok(());
            }
        };
        let id = display.id;
        info!("{peer}: second display {w}×{h}{} (virtual display {id})", if hidpi { " Retina" } else { "" });
        EXTENDED.lock().unwrap().push((session, display));
        // Screen capture sees a new display a moment after it appears.
        for _ in 0..30 {
            let shared = ctx.shared.clone();
            let listed = tokio::task::spawn_blocking(move || shared.video.displays())
                .await?
                .is_ok_and(|ds| ds.iter().any(|d| d.id == id));
            if listed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        set_layout(ctx, Layout::Single(Some(id))).await
    }
}

/// When a session ends, the display it added goes too.
fn end_extend(session: u64, shared: Arc<Shared>) {
    #[cfg(target_os = "macos")]
    {
        let mine: Vec<_> = {
            let mut list = EXTENDED.lock().unwrap();
            let (mine, rest): (Vec<_>, Vec<_>) = list.drain(..).partition(|(s, _)| *s == session);
            *list = rest;
            mine
        };
        if mine.is_empty() {
            return;
        }
        WINDOW_WATCH.fetch_add(1, Ordering::Relaxed);
        tokio::task::spawn_blocking(move || {
            let gone: Vec<u32> = mine.iter().map(|(_, d)| d.id).collect();
            drop(mine);
            // Only move the others off it if it was what they were watching.
            let streaming_it = shared.video.streams().iter().any(|d| gone.contains(&d.id));
            if streaming_it && let Ok(streams) = shared.video.set_layout(Layout::Single(None)) {
                let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
                shared.injector.lock().unwrap().set_displays(&ids);
                let displays = shared.video.displays().ok();
                let _ = shared.events.send(ServerMsg::Layout { streams, displays });
            }
            info!("second display removed (its viewer left)");
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (session, shared);
}

/// When a session ends, its "fit this device" display goes too.
fn end_fit(session: u64, shared: Arc<Shared>) {
    #[cfg(target_os = "macos")]
    {
        let mut fit = FIT.lock().unwrap();
        if fit.as_ref().is_some_and(|f| f.owner == session) {
            let old = fit.take();
            WINDOW_WATCH.fetch_add(1, Ordering::Relaxed);
            tokio::task::spawn_blocking(move || {
                drop(old);
                if let Ok(streams) = shared.video.set_layout(Layout::Single(None)) {
                    let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
                    shared.injector.lock().unwrap().set_displays(&ids);
                    let displays = shared.video.displays().ok();
                    let _ = shared.events.send(ServerMsg::Layout { streams, displays });
                }
                info!("displays restored (the viewer who fitted them left)");
            });
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (session, shared);
}

/// Bumped on every layout change, so a streamed window's watcher knows it's done.
static WINDOW_WATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// While a window is streamed: follow it when it moves (input mapping), and go back to the
/// main display when it closes.
async fn watch_window(shared: Arc<Shared>, id: u32, generation: u64) {
    let mut last = None;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if WINDOW_WATCH.load(Ordering::Relaxed) != generation {
            return;
        }
        let video = shared.clone();
        let Ok(Ok(windows)) = tokio::task::spawn_blocking(move || video.video.windows()).await
        else {
            continue;
        };
        if WINDOW_WATCH.load(Ordering::Relaxed) != generation {
            return;
        }
        match windows.iter().find(|w| w.id == id) {
            Some(w) => {
                let rect = [w.x, w.y, w.points_width, w.points_height];
                if last.is_some_and(|l| l != rect) {
                    shared.injector.lock().unwrap().set_window_rect(Some(rect));
                }
                last = Some(rect);
            }
            None => {
                info!("window {id} closed; back to the main display");
                WINDOW_WATCH.fetch_add(1, Ordering::Relaxed);
                let video = shared.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let streams = video.video.set_layout(Layout::Single(None))?;
                    anyhow::Ok((streams, video.video.displays().ok()))
                })
                .await;
                if let Ok(Ok((streams, displays))) = result {
                    let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
                    {
                        let mut injector = shared.injector.lock().unwrap();
                        injector.set_displays(&ids);
                        injector.set_window_rect(None);
                    }
                    let _ = shared.events.send(ServerMsg::Layout { streams, displays });
                }
                let msg = "The window you were viewing closed, so here's the main display.".into();
                let _ = shared.events.send(ServerMsg::Error { msg });
                return;
            }
        }
    }
}

async fn set_layout(ctx: &SessionCtx, layout: Layout) -> Result<()> {
    let worker = ctx.shared.clone();
    let peer = ctx.conn.remote_address();
    info!("{peer}: layout {layout:?}");
    let listed = tokio::task::spawn_blocking(move || {
        let streams = worker.video.set_layout(layout)?;
        anyhow::Ok((streams, worker.video.displays().ok()))
    });
    match listed.await? {
        Ok((streams, displays)) => {
            let ids: Vec<u32> = streams.iter().map(|d| d.id).collect();
            info!("{peer}: now streaming displays {ids:?}");
            {
                let mut injector = ctx.shared.injector.lock().unwrap();
                injector.set_displays(&ids);
                // A window: absolute coordinates on stream 0 land inside it.
                let rect = match (layout, streams.first()) {
                    (Layout::Window(_), Some(d)) => Some([d.x, d.y, d.points_width, d.points_height]),
                    _ => None,
                };
                injector.set_window_rect(rect);
            }
            let generation = WINDOW_WATCH.fetch_add(1, Ordering::Relaxed) + 1;
            if let Layout::Window(id) = layout {
                tokio::spawn(watch_window(ctx.shared.clone(), id, generation));
            }
            let _ = ctx.shared.events.send(ServerMsg::Layout { streams, displays });
        }
        Err(e) => {
            warn!("{peer}: switch display failed: {e:#}");
            let _ = ctx.ctl_tx.send(ServerMsg::Error {
                msg: format!("switch display: {e:#}"),
            });
        }
    }
    Ok(())
}

/// Failed sign-ins allowed per address within `AUTH_WINDOW` before it's locked out.
/// How a session signed in.
enum Auth {
    Token,
    Tailnet,
    /// Someone else's tailnet device, let in by the access rules or `tailnet-members`.
    TailnetGuest {
        login: String,
        scope: crate::access::Scope,
    },
    /// Via a magic link: `new` when the link was opened just now (the pass is sent back).
    Guest {
        pass: String,
        expires: u64,
        new: bool,
        scope: crate::access::Scope,
        passkey: Option<crate::access::Passkey>,
    },
}

/// What someone else on the tailnet (`login`) may do: the access rules' first match, else a
/// guest's default if the owner lets the whole tailnet in; `None`: they need the token.
fn tailnet_guest_scope(login: &str) -> Option<crate::access::Scope> {
    if let Some((scope, _)) = crate::acl::decide(&crate::acl::effective(), login, &[]) {
        return Some(scope);
    }
    crate::settings::load().tailnet_members.then(crate::access::Scope::default)
}

/// How often a guest session re-checks that its pass is still valid.
const GUEST_RECHECK: Duration = Duration::from_secs(10);

/// A client's keyframe requests are honoured at most this often (per stream).
const KEYFRAME_REQUEST_SPACING: Duration = Duration::from_millis(500);
/// A session that hasn't heard from its page for this long is ended.
const QUIET_LIMIT: Duration = Duration::from_secs(15);
const QUIET_CHECK: Duration = Duration::from_secs(3);

const AUTH_MAX_FAILURES: u32 = 10;
const AUTH_WINDOW: Duration = Duration::from_secs(600);

fn auth_blocked(shared: &Shared, peer: std::net::IpAddr) -> bool {
    let mut failures = shared.auth_failures.lock().unwrap();
    failures.retain(|_, (_, since)| since.elapsed() < AUTH_WINDOW);
    failures
        .get(&peer)
        .is_some_and(|(n, _)| *n >= AUTH_MAX_FAILURES)
}

fn record_auth_failure(shared: &Shared, peer: std::net::IpAddr) {
    let mut failures = shared.auth_failures.lock().unwrap();
    let entry = failures
        .entry(peer)
        .or_insert((0, std::time::Instant::now()));
    entry.0 += 1;
}

const AUTH_WAIT: Duration = Duration::from_secs(10);

/// Input streams open right now, across viewers.
static INPUT_STREAMS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

async fn uni_stream(ctx: SessionCtx, mut stream: RecvStream) -> Result<()> {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).await?;
    // Streams opened right after hello can arrive while auth is still in progress
    // (a tailnet whois takes tens of ms); wait for it rather than dropping them.
    let deadline = tokio::time::Instant::now() + AUTH_WAIT;
    while !ctx.authed.load(Ordering::Relaxed) {
        if tokio::time::Instant::now() > deadline {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let perms = ctx.perms.get().copied().unwrap_or(control::Perms {
        input: false,
        clipboard: false,
        files: false,
        owner: false,
    });
    match tag[0] {
        STREAM_INPUT => {
            info!("{}: input stream open", ctx.conn.remote_address());
            INPUT_STREAMS.fetch_add(1, Ordering::Relaxed);
            let mut buf = [0u8; INPUT_EVENT_LEN];
            let mut events = 0u64;
            // What this viewer is holding down, to let go of when it leaves.
            let mut keys = std::collections::HashSet::new();
            let mut buttons = std::collections::HashSet::new();
            while stream.read_exact(&mut buf).await.is_ok() {
                // A view-only link's input is read and dropped.
                if !perms.input {
                    continue;
                }
                match InputEvent::read(&buf) {
                    Some(ev @ InputEvent::Gamepad { .. }) if !crate::testmode::enabled() => {
                        crate::gamepad::handle(ev)
                    }
                    Some(ev) => {
                        // A person clicking, typing or scrolling: agents wait for them.
                        if matches!(ev, InputEvent::MouseDown { .. } | InputEvent::KeyDown { .. } | InputEvent::Scroll { .. }) {
                            crate::computer::human_input();
                        }
                        match ev {
                            InputEvent::KeyDown { keycode } => _ = keys.insert(keycode),
                            InputEvent::KeyUp { keycode } => _ = keys.remove(&keycode),
                            InputEvent::MouseDown { button } => _ = buttons.insert(button),
                            InputEvent::MouseUp { button } => _ = buttons.remove(&button),
                            _ => {}
                        }
                        ctx.shared.injector.lock().unwrap().handle(ev)
                    }
                    None => continue,
                }
                events += 1;
            }
            {
                let mut injector = ctx.shared.injector.lock().unwrap();
                for keycode in keys {
                    injector.handle(InputEvent::KeyUp { keycode });
                }
                for button in buttons {
                    injector.handle(InputEvent::MouseUp { button });
                }
            }
            INPUT_STREAMS.fetch_sub(1, Ordering::Relaxed);
            info!(
                "{}: input stream closed after {events} events",
                ctx.conn.remote_address()
            );
        }
        STREAM_CLIPBOARD if !perms.clipboard => {
            warn!("{}: clipboard is off for this link; ignored", ctx.conn.remote_address());
        }
        STREAM_CLIPBOARD => {
            let (clip, paste) = crate::clip::receive(&mut stream).await?;
            info!(
                "{}: clipboard with an image ({} bytes){}",
                ctx.conn.remote_address(),
                clip.image.as_ref().map_or(0, Vec::len),
                if paste { ", pasting" } else { "" }
            );
            let clipboard = ctx.shared.clipboard.clone();
            tokio::task::spawn_blocking(move || clipboard.set(&clip)).await?;
            if paste && perms.input {
                ctx.shared.injector.lock().unwrap().paste();
            }
        }
        STREAM_FILE if !perms.files => {
            let _ = ctx.ctl_tx.send(ServerMsg::Error {
                msg: "Files are turned off for this link.".into(),
            });
        }
        STREAM_FILE => {
            let (name, path) = files::receive(&mut stream, perms.owner).await?;
            let _ = ctx.ctl_tx.send(ServerMsg::FileSaved {
                name,
                path: path.display().to_string(),
            });
        }
        other => warn!("unknown stream type {other:#x}"),
    }
    Ok(())
}

/// Frame ids are per session so the client sees a gap only when this session lost a frame.
fn send_frame(
    conn: &wtransport::Connection,
    frame: &EncodedFrame,
    next_id: &mut u32,
) -> Result<()> {
    let max = conn.max_datagram_size().unwrap_or(1200);
    let id = *next_id;
    *next_id = next_id.wrapping_add(1);
    // Test-mode fault injection: a dropped frame leaves a gap in the ids.
    if crate::testmode::take_drop() {
        return Ok(());
    }
    let flags = if frame.keyframe {
        glass_proto::FLAG_KEYFRAME
    } else {
        0
    } | if frame.hevc { glass_proto::FLAG_HEVC } else { 0 }
        | if frame.refresh {
            glass_proto::FLAG_REFRESH
        } else {
            0
        };
    let mut dgrams = glass_proto::fragment_with_flags(
        &frame.data,
        frame.stream,
        id,
        flags,
        frame.capture_us,
        max,
    );
    if crate::testmode::take_corrupt()
        && let Some(last) = dgrams.last_mut()
        && let Some(byte) = last.last_mut()
    {
        *byte ^= 0xff;
    }
    for dgram in dgrams {
        conn.send_datagram(dgram)?;
    }
    Ok(())
}

#[derive(serde::Serialize, Clone)]
struct MachineEntry {
    #[serde(flatten)]
    machine: tailscale::Machine,
    /// Glass answered on this machine (so the client can connect to it).
    glass: bool,
    /// Its WebTransport port, from its /config.json.
    wt_port: Option<u16>,
    /// Its MAC addresses, from /config.json now or remembered from before (Wake-on-LAN).
    macs: Vec<String>,
}

type MachinesCache = Arc<tokio::sync::Mutex<Option<(std::time::Instant, Vec<MachineEntry>)>>>;
const MACHINES_TTL: Duration = Duration::from_secs(15);

/// Your tailnet devices and which of them run Glass, for the client's machine switcher.
/// The web app's manifest, with a shortcut to each other machine running Glass (long-press
/// the app icon: "Mac Studio").
async fn manifest(web_dir: PathBuf, cache: MachinesCache, port: u16, wt_port: u16) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Ok(mut m) = tokio::fs::read(web_dir.join("manifest.webmanifest"))
        .await
        .map_err(anyhow::Error::from)
        .and_then(|b| Ok(serde_json::from_slice::<serde_json::Value>(&b)?))
    else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    // Don't hold the manifest up on probing the tailnet.
    let list = tokio::time::timeout(Duration::from_secs(3), machines(cache, port, wt_port)).await;
    if let (Ok(Json(list)), Some(shortcuts)) = (list, m["shortcuts"].as_array_mut()) {
        let others = list["machines"].as_array().cloned().unwrap_or_default();
        for o in others.iter().filter(|o| o["glass"] == true && o["self"] != true).take(4) {
            let dns = o["dns"].as_str().unwrap_or_default();
            let id = match o["wt_port"].as_u64() {
                Some(p) if p != wt_port as u64 => format!("{dns}:{p}"),
                _ => dns.to_string(),
            };
            shortcuts.push(serde_json::json!({
                "name": o["name"],
                "url": format!("./?machine={id}"),
                "icons": [{ "src": "icon-192.png", "sizes": "192x192" }],
            }));
        }
    }
    (
        [(header::CONTENT_TYPE, HeaderValue::from_static("application/manifest+json"))],
        Json(m),
    )
        .into_response()
}

async fn machines(cache: MachinesCache, port: u16, wt_port: u16) -> Json<serde_json::Value> {
    // Tests stand in other hosts for tailnet machines.
    if crate::testmode::enabled()
        && let Ok(list) = std::env::var("GLASS_TEST_MACHINES")
        && let Ok(list) = serde_json::from_str::<serde_json::Value>(&list)
    {
        return Json(serde_json::json!({ "machines": list, "groups": [] }));
    }
    let mut cache = cache.lock().await;
    if let Some((at, list)) = cache.as_ref()
        && at.elapsed() < MACHINES_TTL
    {
        return Json(serde_json::json!({ "machines": list, "groups": crate::admin::load_groups().groups }));
    }
    let list = match tailscale::machines().await {
        Ok(found) => {
            // Probe every machine at once; offline ones are skipped.
            let probes: Vec<_> = found
                .into_iter()
                .map(|m| {
                    tokio::spawn(async move {
                        let (wt_port, macs) = if m.is_self {
                            (Some(wt_port), Vec::new())
                        } else if m.online {
                            probe_glass(&m.dns, port).await.map_or((None, Vec::new()), |(p, macs)| (Some(p), macs))
                        } else {
                            (None, Vec::new())
                        };
                        if !macs.is_empty() {
                            crate::wol::remember(&m.dns, &macs);
                        }
                        let macs = if macs.is_empty() && !m.is_self {
                            crate::wol::known().remove(&m.dns).unwrap_or_default()
                        } else {
                            macs
                        };
                        MachineEntry {
                            glass: wt_port.is_some(),
                            machine: m,
                            wt_port,
                            macs,
                        }
                    })
                })
                .collect();
            let mut list = Vec::with_capacity(probes.len());
            for p in probes {
                if let Ok(entry) = p.await {
                    list.push(entry);
                }
            }
            list
        }
        Err(e) => {
            warn!("machines: {e:#}");
            Vec::new()
        }
    };
    *cache = Some((std::time::Instant::now(), list.clone()));
    Json(serde_json::json!({ "machines": list, "groups": crate::admin::load_groups().groups }))
}

/// This computer's name (looked up once: it's a process on macOS).
fn computer_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(crate::support::computer_name)
}

/// If a Glass host answers at `https://<dns>:<port>/config.json`, its WebTransport port.
/// Tries the port-less address first (it's there even when a host's web port had to move),
/// then the usual port.
async fn probe_glass(dns: &str, port: u16) -> Option<(u16, Vec<String>)> {
    for url in [
        format!("https://{dns}/config.json"),
        format!("https://{dns}:{port}/config.json"),
    ] {
        let Ok(out) = tokio::process::Command::new("curl")
            .args(["-fsS", "--max-time", "1.5", &url])
            .output()
            .await
        else {
            continue;
        };
        let config: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
        if let Some(c) = config
            && let Some(port) = c["wtPort"].as_u64()
        {
            let macs = c["macs"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|m| m.as_str().map(String::from))
                .collect();
            return Some((port as u16, macs));
        }
    }
    None
}

/// Browsers accept a self-signed cert via `serverCertificateHashes` only if it is ECDSA
/// and valid for at most 14 days. It's saved and reused (and replaced before it expires)
/// so its hash stays stable across restarts, and hosts sharing a config dir share it.
async fn self_signed_identity(wt_host: Option<&str>) -> Result<Identity> {
    const MAX_AGE: Duration = Duration::from_secs(10 * 86400);
    let dir = crate::config_dir().join("certs");
    let (cert, key) = (dir.join("self-signed.crt"), dir.join("self-signed.key"));
    let fresh = std::fs::metadata(&cert)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().unwrap_or(MAX_AGE) < MAX_AGE);
    if fresh && let Ok(identity) = Identity::load_pemfiles(&cert, &key).await {
        return Ok(identity);
    }
    let names = ["localhost", "127.0.0.1", "::1"]
        .map(String::from)
        .into_iter()
        .chain(wt_host.map(String::from));
    let identity = Identity::self_signed(names)?;
    tokio::fs::create_dir_all(&dir).await?;
    identity.certificate_chain().store_pemfile(&cert).await?;
    identity.private_key().store_secret_pemfile(&key).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600));
    }
    Ok(identity)
}

/// Changes whenever a web client file changes, so open tabs know to reload.
fn web_version(dir: &std::path::Path) -> String {
    let newest = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default();
    format!("{}", newest.as_millis())
}

/// Client log batches arrive as JSON lines; appended verbatim with a receive timestamp.
/// Feeds one congestion report (from a client, or the WebRTC bridge) to the adaptive bitrate.
fn apply_rate_report(shared: &Shared, report: crate::rate::Report) {
    let changed = shared
        .rate
        .lock()
        .unwrap()
        .report(report, std::time::Instant::now());
    if changed.is_some() {
        apply_bitrate(shared);
    }
}

/// Each remote (RTP) viewer's congestion-control estimate, as a cap on the shared encoders.
static REMOTE_CAPS: Mutex<Option<std::collections::HashMap<u64, u32>>> = Mutex::new(None);
static APPLIED_BPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Sets (or with `None` removes) a remote viewer's bitrate cap.
pub fn set_remote_cap(viewer: u64, bps: Option<u32>) {
    {
        let mut caps = REMOTE_CAPS.lock().unwrap();
        let caps = caps.get_or_insert_with(Default::default);
        match bps {
            Some(b) => caps.insert(viewer, b),
            None => caps.remove(&viewer),
        };
    }
    if let Some(shared) = SHARED.get() {
        apply_bitrate(shared);
    }
}

/// The encoders' bitrate: the tailnet clients' adaptive target, capped by every remote
/// viewer's estimate (they share the encoders, so the slowest link sets the rate).
fn apply_bitrate(shared: &Shared) {
    let aimd = shared.rate.lock().unwrap().target();
    let cap = REMOTE_CAPS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|c| c.values().min().copied());
    let bps = cap.map_or(aimd, |c| c.min(aimd));
    let last = APPLIED_BPS.load(Ordering::Relaxed);
    // Ignore wiggles under 3% so encoders aren't poked twice a second.
    if last != 0 && (bps as f64 - last as f64).abs() < last as f64 * 0.03 {
        return;
    }
    APPLIED_BPS.store(bps, Ordering::Relaxed);
    // Log real moves only: congestion control nudges the rate every half second.
    static LOGGED_BPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let logged = LOGGED_BPS.load(Ordering::Relaxed);
    if logged == 0 || (bps as f64 - logged as f64).abs() >= logged as f64 * 0.15 {
        LOGGED_BPS.store(bps, Ordering::Relaxed);
        let why = if cap.is_some_and(|c| c < aimd) {
            " (remote link)"
        } else {
            ""
        };
        info!("bitrate → {:.1} Mbps{why}", bps as f64 / 1e6);
    }
    shared.video.set_bitrate(bps);
    let _ = shared.events.send(ServerMsg::Bitrate { bps });
}

/// Re-applies the current bitrate, so encoders pick up a changed policy (sharp text).
pub fn reapply_policy() {
    if let Some(shared) = SHARED.get() {
        let bps = match APPLIED_BPS.load(Ordering::Relaxed) {
            0 => shared.rate.lock().unwrap().target(),
            b => b,
        };
        shared.video.set_bitrate(bps);
    }
}

pub fn shared() -> Option<Arc<Shared>> {
    SHARED.get().cloned()
}

static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();

/// The WebRTC bridge drops whole frames when its link backs up; the browser only sees
/// skipped frames, never damaged ones, so it reports no loss. The bridge reports it here.
pub fn bridge_congestion(dropped_frames: u32) {
    if let Some(shared) = SHARED.get() {
        let report = crate::rate::Report {
            dropped: dropped_frames,
            frames: 1,
            latency_ms: None,
            received_bps: None,
        };
        apply_rate_report(shared, report);
    }
}

/// A viewer arrived over the internet (WebRTC): start the stream at a rate such links
/// usually take, instead of the LAN maximum, so the first keyframe isn't lost.
pub fn remote_viewer_joined() {
    if let Some(shared) = SHARED.get() {
        let max = shared.rate.lock().unwrap().max();
        let start = (max / 5).max(2_000_000);
        let changed = shared
            .rate
            .lock()
            .unwrap()
            .start_cautiously(start, std::time::Instant::now());
        if changed.is_some() {
            apply_bitrate(shared);
        }
    }
}

/// Where client logs go; set once the server starts.
static CLIENT_LOG: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// Caps on log lines arriving over the control stream (remote clients).
const LOG_LINES_MAX: usize = 200;
const LOG_LINE_MAX: usize = 4096;

async fn append_client_log(path: PathBuf, body: String) {
    use std::io::Write;
    let now = unix_now();
    let lines: String = body.lines().map(|l| format!("{now} {l}\n")).collect();
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(lines.as_bytes()));
    if let Err(e) = result {
        warn!("writing client log: {e}");
    }
    for line in body.lines().filter(|l| l.contains("\"level\":\"error\"")) {
        warn!(target: "client", "{line}");
    }
}

fn unix_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn identity() -> Identity {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Identity::self_signed(["localhost"]).unwrap()
    }

    #[tokio::test]
    async fn webtransport_binds_the_chosen_address_and_skips_a_taken_port() {
        let identity = identity();
        let listen = crate::listen::Listen::Explicit(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
        let hold = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let taken = hold.local_addr().unwrap().port();
        let (endpoints, port) = bind_wt(&identity, &listen, taken).unwrap();
        assert_eq!(endpoints.len(), 1);
        assert!(port > taken && port < taken.saturating_add(crate::PORT_TRIES), "{port}");
        assert_eq!(endpoints[0].local_addr().unwrap().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(endpoints[0].local_addr().unwrap().port(), port);
    }
}
