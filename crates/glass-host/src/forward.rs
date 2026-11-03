//! Port forwarding: an owner's device opens `/fwd/<port>/…` on the host's web server and
//! gets that local port (say a dev server on 3000), without exposing it to the network.
//! Only ports listed in `glass config forward-ports` work, and only for the owner: one of
//! their own tailnet devices, or a browser that proved it has the access token
//! (`/fwd/auth?t=…` sets a cookie). Plain HTTP and WebSockets are proxied.

use std::net::{IpAddr, SocketAddr};

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, Request};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use hyper_util::rt::TokioIo;
use tracing::{info, warn};

/// Routes merged into the web server.
pub fn routes() -> axum::Router {
    axum::Router::new()
        .route("/fwd/auth", get(auth))
        .route("/fwd/{port}", any(|Path(port): Path<u16>| async move { redirect(&format!("/fwd/{port}/"), None) }))
        .route("/fwd/{port}/", any(|ci, Path(port): Path<u16>, req| proxy(ci, port, String::new(), req)))
        .route(
            "/fwd/{port}/{*rest}",
            any(|ci, Path((port, rest)): Path<(u16, String)>, req| proxy(ci, port, rest, req)),
        )
}

pub(crate) const COOKIE: &str = "glass_fwd";

/// The cookie value that proves the token: an HMAC of it, so the token itself never sits
/// in a cookie. Changes when the token does (`glass token --reset` logs browsers out).
pub(crate) fn cookie_value(token: &str) -> String {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, token.as_bytes());
    let tag = ring::hmac::sign(&key, b"glass port forwarding");
    tag.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn token() -> Option<String> {
    let shared = crate::server::shared()?;
    crate::control::load_or_create_token(&shared.token_file)
        .ok()
        .filter(|t| !t.is_empty())
}

/// The `glass_fwd` cookie from a request, if any.
fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

/// Every cookie but ours, for the forwarded request.
fn other_cookies(headers: &HeaderMap) -> Option<String> {
    let rest: Vec<&str> = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .map(str::trim)
        .filter(|c| !c.is_empty() && !c.starts_with(&format!("{COOKIE}=")))
        .collect();
    (!rest.is_empty()).then(|| rest.join("; "))
}

/// The computer's owner: an owner tailnet device, the access token as a bearer header,
/// or the signed cookie `/fwd/auth` (or `/admin/auth`) sets. Shared by other owner-only
/// HTTP routes.
pub(crate) async fn is_owner(peer: IpAddr, headers: &HeaderMap) -> bool {
    // Tests stand a second local host in for another tailnet machine of the same owner.
    if crate::testmode::enabled() && peer.is_loopback() && std::env::var_os("GLASS_TEST_TRUST_LOOPBACK").is_some() {
        return true;
    }
    if let (Some(auth), Some(token)) = (headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()), token())
        && let Some(given) = auth.strip_prefix("Bearer ")
        && crate::control::token_matches(given.trim(), &token)
    {
        return true;
    }
    if let (Some(given), Some(token)) = (cookie(headers), token())
        && crate::control::token_matches(&given, &cookie_value(&token))
    {
        return true;
    }
    // Same status snapshot as a new session: Self's UserID and this peer's, together.
    let Some(shared) = crate::server::shared() else {
        return false;
    };
    matches!(
        shared.tailnet_owner.view(peer).await,
        crate::tailscale::View::Ready { is_owner: true, .. }
    )
}

#[derive(serde::Deserialize)]
struct AuthQuery {
    t: String,
    #[serde(default)]
    to: Option<String>,
}

/// `/fwd/auth?t=<token>&to=/fwd/3000/`: sets the cookie and goes on to `to`.
async fn auth(Query(q): Query<AuthQuery>) -> Response<Body> {
    let Some(token) = token() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !crate::control::token_matches(&q.t, &token) {
        // Slows guessing down; the WebTransport sign-in has its own lockout.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        return (StatusCode::FORBIDDEN, "wrong token").into_response();
    }
    let to = q
        .to
        .filter(|t| t.starts_with("/fwd/") && !t.contains("//") && !t.contains('\\'))
        .unwrap_or_else(|| "/fwd/".into());
    let cookie = format!(
        "{COOKIE}={}; Path=/fwd; HttpOnly; SameSite=Lax; Max-Age=2592000",
        cookie_value(&token)
    );
    redirect(&to, Some(cookie))
}

fn redirect(to: &str, cookie: Option<String>) -> Response<Body> {
    let mut r = Response::builder()
        .status(StatusCode::SEE_OTHER)
        .header(header::LOCATION, to);
    if let Some(c) = cookie {
        r = r.header(header::SET_COOKIE, c);
    }
    r.body(Body::empty()).unwrap()
}

/// Headers that belong to one hop, never forwarded (RFC 9110 §7.6.1).
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Drops hop-by-hop headers (and any the Connection header names), except an upgrade
/// that's being passed through.
fn strip_hop_by_hop(headers: &mut HeaderMap, keep_upgrade: bool) {
    let named: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|n| HeaderName::try_from(n.trim()).ok())
        .collect();
    let upgrade = headers.get(header::UPGRADE).cloned();
    for n in named {
        headers.remove(n);
    }
    for n in HOP_BY_HOP {
        headers.remove(*n);
    }
    if keep_upgrade && let Some(u) = upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, u);
    }
}

/// A redirect from the forwarded app, kept under `/fwd/<port>/`.
fn rewrite_location(location: &str, port: u16) -> String {
    let prefix = format!("/fwd/{port}");
    for origin in [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
    ] {
        if let Some(rest) = location.strip_prefix(&origin) {
            return format!("{prefix}{}", if rest.is_empty() { "/" } else { rest });
        }
    }
    if location.starts_with('/') && !location.starts_with("//") && !location.starts_with(&prefix) {
        return format!("{prefix}{location}");
    }
    location.to_string()
}

/// Is `port` one the owner chose to forward?
fn allowed(port: u16, ports: &[u16]) -> bool {
    ports.contains(&port)
}

async fn proxy(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    port: u16,
    rest: String,
    mut req: Request,
) -> Response<Body> {
    if !allowed(port, &crate::settings::load().forward_ports) {
        return (
            StatusCode::NOT_FOUND,
            format!("Port {port} isn't forwarded. On the host: glass config forward-ports {port}"),
        )
            .into_response();
    }
    let peer_ip = peer.ip().to_canonical();
    if !is_owner(peer_ip, req.headers()).await {
        warn!("{peer_ip}: refused forwarded port {port}");
        return (StatusCode::FORBIDDEN, "Only this computer's owner can open forwarded ports.")
            .into_response();
    }
    let wants_upgrade = req.headers().contains_key(header::UPGRADE);
    let client_upgrade = wants_upgrade.then(|| hyper::upgrade::on(&mut req));

    let path_and_query = match req.uri().query() {
        Some(q) => format!("/{rest}?{q}"),
        None => format!("/{rest}"),
    };
    let (mut parts, body) = req.into_parts();
    let cookies = other_cookies(&parts.headers);
    parts.headers.remove(header::COOKIE);
    if let Some(c) = cookies.and_then(|c| HeaderValue::from_str(&c).ok()) {
        parts.headers.insert(header::COOKIE, c);
    }
    strip_hop_by_hop(&mut parts.headers, wants_upgrade);
    parts.headers.insert(
        header::HOST,
        HeaderValue::from_str(&format!("127.0.0.1:{port}")).unwrap(),
    );
    parts.uri = match path_and_query.parse() {
        Ok(u) => u,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let upstream_req = axum::http::Request::from_parts(parts, body);

    let stream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("Nothing answered on port {port}: {e}"))
                .into_response();
        }
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(pair) => pair,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            info!("forwarded port {port}: {e}");
        }
    });
    let mut resp = match sender.send_request(upstream_req).await {
        Ok(r) => r,
        Err(e) => return (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    };

    if resp.status() == StatusCode::SWITCHING_PROTOCOLS
        && let Some(client_upgrade) = client_upgrade
    {
        let upstream_upgrade = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            match (client_upgrade.await, upstream_upgrade.await) {
                (Ok(client), Ok(upstream)) => {
                    let (mut a, mut b) = (TokioIo::new(client), TokioIo::new(upstream));
                    let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                }
                (a, b) => warn!(
                    "forwarded port {port}: upgrade failed ({:?}, {:?})",
                    a.err(),
                    b.err()
                ),
            }
        });
        let (parts, _) = resp.into_parts();
        return Response::from_parts(parts, Body::empty());
    }

    let (mut parts, body) = resp.into_parts();
    strip_hop_by_hop(&mut parts.headers, false);
    if let Some(loc) = parts.headers.get(header::LOCATION).and_then(|l| l.to_str().ok())
        && let Ok(v) = HeaderValue::from_str(&rewrite_location(loc, port))
    {
        parts.headers.insert(header::LOCATION, v);
    }
    Response::from_parts(parts, Body::new(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_ports() {
        assert!(allowed(3000, &[3000, 8888]));
        assert!(!allowed(22, &[3000, 8888]));
        assert!(!allowed(3000, &[]));
    }

    #[test]
    fn locations_stay_under_the_prefix() {
        assert_eq!(rewrite_location("/login", 3000), "/fwd/3000/login");
        assert_eq!(rewrite_location("http://localhost:3000/a?b", 3000), "/fwd/3000/a?b");
        assert_eq!(rewrite_location("http://127.0.0.1:3000", 3000), "/fwd/3000/");
        assert_eq!(rewrite_location("/fwd/3000/x", 3000), "/fwd/3000/x");
        assert_eq!(rewrite_location("https://example.com/", 3000), "https://example.com/");
        assert_eq!(rewrite_location("//evil.example/", 3000), "//evil.example/");
        assert_eq!(rewrite_location("next", 3000), "next");
    }

    #[test]
    fn hop_by_hop_headers_are_dropped() {
        let mut h = HeaderMap::new();
        h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive, x-secret"));
        h.insert("x-secret", HeaderValue::from_static("1"));
        h.insert(header::TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        h.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
        let mut plain = h.clone();
        strip_hop_by_hop(&mut plain, false);
        assert_eq!(plain.len(), 1);
        assert!(plain.contains_key(header::ACCEPT));
        strip_hop_by_hop(&mut h, true);
        assert_eq!(h.get(header::UPGRADE).unwrap(), "websocket");
        assert_eq!(h.get(header::CONNECTION).unwrap(), "upgrade");
        assert!(!h.contains_key("x-secret"));
    }

    #[test]
    fn our_cookie_is_found_and_not_forwarded() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_static("a=1; glass_fwd=abc; b=2"));
        assert_eq!(cookie(&h).as_deref(), Some("abc"));
        assert_eq!(other_cookies(&h).as_deref(), Some("a=1; b=2"));
        assert_ne!(cookie_value("t1"), cookie_value("t2"));
        assert_eq!(cookie_value("t1").len(), 64);
    }
}
