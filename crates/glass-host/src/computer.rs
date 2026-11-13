//! Agents driving this computer: screenshots, pointer and keyboard, waiting for the screen,
//! and asking the person for help or a yes/no. Owner-only HTTP at `/agent/v1/*` (an owner
//! tailnet device, or the token as a bearer header); `glass mcp` wraps it for MCP clients.
//!
//! Everything an agent does is shown to the people viewing: an "agent working" banner with
//! Pause and Stop, a marker where it points, and its questions as cards to answer.
//! Paused, actions wait; stopped, they're refused until the owner allows it again.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine;
use glass_proto::InputEvent;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tracing::info;

use crate::control::ServerMsg;
use crate::media::DisplayInfo;

/// Screenshot size when the agent doesn't ask for one (what computer-use models expect).
pub const DEFAULT_MAX: [u32; 2] = [1280, 800];
/// The largest screenshot an agent can ask for.
const LARGEST: u32 = 2560;
/// How long a paused agent's action waits before giving up.
const PAUSE_WAIT: Duration = Duration::from_secs(600);
/// How long a question waits for the person.
const ASK_WAIT: Duration = Duration::from_secs(1800);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Default)]
pub struct Status {
    pub paused: bool,
    pub stopped: bool,
}

struct Ask {
    kind: &'static str,
    text: String,
    /// Answers to pick from (the number an agent's sign-in page shows, among others).
    choices: Option<Vec<String>>,
    reply: oneshot::Sender<Answer>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Answer {
    pub ok: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Default)]
struct State {
    status: Status,
    /// Size of each agent's last screenshot of each display: its x, y are in those pixels.
    sizes: HashMap<(String, u32), (u32, u32)>,
    asks: HashMap<u64, Ask>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(STATE.lock().unwrap().get_or_insert_with(|| State { status: saved_status(), ..Default::default() }))
}

/// Stop and Pause outlast a restart (an update, a crash): a stopped agent stays stopped.
fn status_path() -> std::path::PathBuf {
    crate::config_dir().join("agent-status.json")
}

fn saved_status() -> Status {
    let v: Value = std::fs::read(status_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    Status { paused: v["paused"] == true, stopped: v["stopped"] == true }
}

fn save_status(status: Status) {
    let _ = std::fs::write(status_path(), json!({ "paused": status.paused, "stopped": status.stopped }).to_string());
}

/// A question's id: random, so one asked before a restart never matches one after.
fn new_ask_id() -> u64 {
    let mut b = [0u8; 8];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).expect("random");
    // Within what JavaScript numbers hold exactly.
    (u64::from_le_bytes(b) >> 12).max(1)
}

fn broadcast(msg: ServerMsg) {
    if let Some(shared) = crate::server::shared() {
        let _ = shared.events.send(msg);
    }
}

/// What a viewer who just joined needs: the status, and questions waiting for an answer.
pub fn snapshot() -> Vec<ServerMsg> {
    with(|s| {
        let mut out = Vec::new();
        if s.status != Status::default() {
            out.push(ServerMsg::AgentStatus { status: s.status });
        }
        let mut asks: Vec<_> = s.asks.iter().collect();
        asks.sort_by_key(|(id, _)| **id);
        for (id, a) in asks {
            out.push(ServerMsg::AgentAsk { id: *id, kind: a.kind, text: a.text.clone(), choices: a.choices.clone() });
        }
        out
    })
}

/// Someone connected through a link (not the owner).
pub fn guest_connected() -> bool {
    crate::viewers::subscribe().borrow().iter().any(|v| matches!(v.how, crate::viewers::How::Guest))
}

/// The owner, from a viewer: `pause`, `resume`, `stop` or `allow` (after a stop).
pub fn control(op: &str) {
    let status = with(|s| {
        match op {
            "pause" => s.status.paused = true,
            "resume" => s.status.paused = false,
            "stop" => {
                s.status = Status { paused: false, stopped: true };
                // Nobody's waiting on answers from an agent that's been stopped.
                // (Agents asking to be let in are a separate matter: they stay.)
                let ids: Vec<u64> = s.asks.iter().filter(|(_, a)| a.kind != "consent").map(|(id, _)| *id).collect();
                for id in ids {
                    if let Some(ask) = s.asks.remove(&id) {
                        let _ = ask.reply.send(Answer { ok: false, note: Some("stopped by the user".into()) });
                        broadcast(ServerMsg::AgentAnswered { id });
                    }
                }
            }
            "allow" => s.status = Status::default(),
            _ => {}
        }
        if matches!(op, "resume" | "allow") {
            // Resuming also gives agents over their hourly limit a fresh hour.
            crate::grants::reset_budgets();
        }
        s.status
    });
    save_status(status);
    info!("agent: {op} (by the owner)");
    crate::testmode::record("agent", json!({ "op": op }));
    broadcast(ServerMsg::AgentStatus { status });
}

pub fn status() -> Status {
    with(|s| s.status)
}

/// Questions waiting for the owner, oldest first.
pub fn asks() -> Vec<Value> {
    with(|s| {
        let mut asks: Vec<_> = s
            .asks
            .iter()
            .map(|(id, a)| json!({ "id": id, "kind": a.kind, "text": a.text, "choices": a.choices }))
            .collect();
        asks.sort_by_key(|a| a["id"].as_u64());
        asks
    })
}

/// The owner answered a question.
pub fn answer(id: u64, answer: Answer) {
    answer_from(id, answer, false);
}

/// When an agent under a grant last acted (ms since the epoch).
static AGENT_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// An answer, from a viewer on this computer itself (`local`) or elsewhere. Letting an agent
/// in is refused when the answer could have come from an agent: from this computer's own
/// screen while an agent is acting or just acted, or while a guest is connected.
pub fn answer_from(id: u64, mut answer: Answer, local: bool) {
    let consent = with(|s| s.asks.get(&id).is_some_and(|a| a.kind == "consent"));
    if consent && answer.ok {
        let agent_busy = epoch_ms().saturating_sub(AGENT_AT.load(std::sync::atomic::Ordering::Relaxed)) < 10_000;
        let why = if local && agent_busy {
            Some("it came from this computer's own screen while an agent was working")
        } else if guest_connected() {
            Some("a guest is connected")
        } else {
            None
        };
        if let Some(why) = why {
            tracing::warn!("agent: consent {id} denied: {why}");
            crate::push::activity("An agent's request was denied", &format!("Approving it was refused because {why}. Try again."), "agent-consent");
            answer = Answer { ok: false, note: None };
        }
    }
    if let Some(ask) = with(|s| s.asks.remove(&id)) {
        info!("agent: question {id} answered ({})", if answer.ok { "yes/done" } else { "no" });
        let _ = ask.reply.send(answer);
        broadcast(ServerMsg::AgentAnswered { id });
    }
}

#[derive(Debug)]
pub struct Refused(pub String);

/// When a person last clicked, typed or scrolled from a viewer (ms since the epoch).
static HUMAN_AT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// An agent waits this long after a person last used the computer.
const HUMAN_QUIET: Duration = Duration::from_secs(5);

fn epoch_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// A person used the computer from a viewer (a click, a key or a scroll; not just moving
/// the pointer): agents hold off until they've been quiet for a few seconds.
pub fn human_input() {
    HUMAN_AT.store(epoch_ms(), std::sync::atomic::Ordering::Relaxed);
}

fn human_busy() -> bool {
    epoch_ms().saturating_sub(HUMAN_AT.load(std::sync::atomic::Ordering::Relaxed)) < HUMAN_QUIET.as_millis() as u64
        || crate::grants::local_input_recent()
}

/// How long an agent from another device waits while paused or while the person uses the
/// computer, before it's told to try again (its request can't stay open for long).
const AGENT_HOLD: Duration = Duration::from_secs(20);

/// Waits while paused or while a person is using the computer (just looking doesn't wait
/// for them); refuses when stopped.
pub(crate) async fn may_act(look: bool) -> Result<(), Refused> {
    // Screenshots taken from the stream (Windows) need the pointer in it.
    crate::cursor::agent_acted();
    let start = Instant::now();
    let hold = if matches!(crate::grants::current(), crate::grants::Actor::Agent(_)) { AGENT_HOLD } else { PAUSE_WAIT };
    let mut said = false;
    loop {
        let status = with(|s| s.status);
        if status.stopped {
            return Err(Refused(
                "The user stopped the agent. Don't continue; tell them what you were doing.".into(),
            ));
        }
        let busy = !look && human_busy();
        if !status.paused && !busy {
            return Ok(());
        }
        if busy && !said {
            said = true;
            announce("waiting", "while you use the computer".into(), None);
        }
        if start.elapsed() > hold {
            return Err(Refused(if status.paused {
                "The user paused agents. Try again in a minute, or wait for them to resume.".into()
            } else {
                "The person is using the computer right now. Try again in a minute.".into()
            }));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

fn shared() -> Result<std::sync::Arc<crate::server::Shared>, Refused> {
    crate::server::shared().ok_or_else(|| Refused("Glass isn't ready".into()))
}

async fn displays() -> Result<Vec<DisplayInfo>, Refused> {
    let shared = shared()?;
    tokio::task::spawn_blocking(move || shared.video.displays())
        .await
        .map_err(|e| Refused(e.to_string()))?
        .map_err(|e| Refused(format!("{e:#}")))
}

/// The display meant: `id`, or the main one.
async fn display(id: Option<u32>) -> Result<DisplayInfo, Refused> {
    let all = displays().await?;
    match id {
        Some(id) => all.into_iter().find(|d| d.id == id).ok_or_else(|| Refused(format!("no display {id}"))),
        None => all.iter().find(|d| d.main).or(all.first()).cloned().ok_or_else(|| Refused("no displays".into())),
    }
}

/// The size of this agent's last screenshot of `d` (or of a default-sized one).
fn shot_size(d: &DisplayInfo) -> (u32, u32) {
    with(|s| s.sizes.get(&(asker(), d.id)).copied()).unwrap_or_else(|| {
        let (w, h) = crate::thumbs::fit(d.width as usize, d.height as usize, DEFAULT_MAX);
        (w as u32, h as u32)
    })
}

/// `x`, `y` in the pixels of the last screenshot of `d` (or of a default-sized one), as 0..1.
fn normalize(d: &DisplayInfo, x: f64, y: f64) -> (f32, f32) {
    let (w, h) = shot_size(d);
    ((x / w.max(1) as f64).clamp(0.0, 1.0) as f32, (y / h.max(1) as f64).clamp(0.0, 1.0) as f32)
}

/// A global point (in points) as the display it's on, where it is in the pixels of this
/// agent's latest screenshot of that display, and where as 0..1.
pub(crate) async fn locate(x: f64, y: f64) -> Option<(u32, f64, f64, f32, f32)> {
    let all = displays().await.ok()?;
    let d = all.iter().find(|d| x >= d.x && y >= d.y && x < d.x + d.points_width && y < d.y + d.points_height)?;
    let (w, h) = shot_size(d);
    let (nx, ny) = ((x - d.x) / d.points_width.max(1.0), (y - d.y) / d.points_height.max(1.0));
    Some((d.id, nx * w as f64, ny * h as f64, nx as f32, ny as f32))
}

pub(crate) fn announce(kind: &str, detail: String, at: Option<(u32, f32, f32)>) {
    let agent = match crate::grants::current() {
        crate::grants::Actor::Agent(g) => Some(g.client_name),
        crate::grants::Actor::Owner => None,
    };
    info!("agent{}: {kind} {detail}", agent.as_deref().map(|a| format!(" ({a})")).unwrap_or_default());
    broadcast(ServerMsg::AgentAction {
        agent,
        kind: kind.into(),
        detail,
        display: at.map(|a| a.0),
        x: at.map(|a| a.1),
        y: at.map(|a| a.2),
    });
}

/// Where the pointer was last put (global points), for checking what a click there reaches.
static POINTER: Mutex<Option<(f64, f64)>> = Mutex::new(None);

async fn inject(events: impl IntoIterator<Item = InputEvent>) -> Result<(), Refused> {
    use crate::grants::{Target, check_input};
    let events: Vec<InputEvent> = events.into_iter().collect();
    if events.iter().any(|e| matches!(e, InputEvent::KeyDown { .. })) {
        check_input(Target::Keys).map_err(Refused)?;
    }
    if events.iter().any(|e| matches!(e, InputEvent::MouseDown { .. } | InputEvent::Scroll { .. })) {
        // Checked again at the press: what's under the pointer may have changed.
        let (x, y) = POINTER.lock().unwrap().unwrap_or((0.0, 0.0));
        check_input(Target::Point(x, y)).map_err(Refused)?;
    }
    if events.iter().any(|e| matches!(e, InputEvent::MouseMove { .. } | InputEvent::MouseMoveBy { .. })) {
        crate::cursor::moved_by(crate::cursor::AGENT);
    }
    let shared = shared()?;
    let target = if events.iter().any(|e| matches!(e, InputEvent::KeyDown { .. })) { Some(Target::Keys) }
        else { POINTER.lock().unwrap().map(|(x,y)| Target::Point(x,y)) };
    shared.injector.local().map_err(Refused)?.events_checked(events, target).map_err(Refused)?.wait().await.map_err(|e| Refused(e.to_string()))
}

async fn move_to(d: &DisplayInfo, x: f32, y: f32) -> Result<(), Refused> {
    let at = (d.x + x as f64 * d.points_width, d.y + y as f64 * d.points_height);
    crate::grants::check_input(crate::grants::Target::Point(at.0, at.1)).map_err(Refused)?;
    *POINTER.lock().unwrap() = Some(at);
    crate::cursor::moved_by(crate::cursor::AGENT);
    shared()?.injector.local().map_err(Refused)?.controls(vec![crate::input_queue::Control::Move { display: d.id, x, y }], Some(crate::grants::Target::Point(at.0, at.1)))
        .map_err(Refused)?.wait().await.map_err(|e| Refused(e.to_string()))
}

/// An agent under a grant doesn't see protected apps (passwords, keychains…), or apps
/// outside its limit, when they're in front.
pub(crate) fn screen_visible() -> Result<(), Refused> {
    let crate::grants::Actor::Agent(g) = crate::grants::current() else { return Ok(()) };
    let Some(front) = crate::grants::frontmost_app() else { return Ok(()) };
    if crate::grants::PROTECTED_APPS.contains(&front.as_str()) {
        return Err(Refused(format!("{front} is in front, and agents don't see it. Ask the person to switch away with request_help.")));
    }
    if let Some(apps) = &g.apps
        && !apps.contains(&front)
    {
        return Err(Refused(format!("This agent only sees {}; {front} is in front.", apps.join(", "))));
    }
    Ok(())
}

pub async fn screenshot(id: Option<u32>, max: Option<[u32; 2]>) -> Result<Value, Refused> {
    may_act(true).await?;
    screen_visible()?;
    let d = display(id).await?;
    let max = max.unwrap_or(DEFAULT_MAX).map(|v| v.clamp(16, LARGEST));
    let id = d.id;
    // Changes from here on are after this picture (a wait can count them with `since`).
    follow(id).await?;
    let seq = crate::damage::seq();
    let shared = shared()?;
    // A source may need a moment for its first picture (Linux keeps them only on demand).
    let frame = tokio::task::spawn_blocking(move || {
        let start = Instant::now();
        loop {
            if let Some(f) = shared.video.picture(id, max) {
                return Some(f);
            }
            if start.elapsed() > Duration::from_secs(2) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .await
    .map_err(|e| Refused(e.to_string()))?
    .ok_or_else(|| Refused("couldn't take a screenshot of that display".into()))?;
    let png = crate::lossless::encode_png(&frame, [0, 0, frame.width, frame.height])
        .map_err(|e| Refused(format!("{e:#}")))?;
    with(|s| s.sizes.insert((asker(), id), (frame.width as u32, frame.height as u32)));
    announce("screenshot", format!("of display {id}"), None);
    Ok(json!({
        "display": id,
        "width": frame.width,
        "height": frame.height,
        "seq": seq,
        "png": base64::engine::general_purpose::STANDARD.encode(png),
    }))
}

/// Follows display `id`'s changes for a while (an agent is looking at it).
async fn follow(id: u32) -> Result<(), Refused> {
    let shared = shared()?;
    tokio::task::spawn_blocking(move || crate::damage::follow(&shared.video, id)).await.map_err(|e| Refused(e.to_string()))
}

fn button_code(name: Option<&str>) -> Result<u8, Refused> {
    Ok(match name.unwrap_or("left") {
        "left" => 0,
        "middle" => 1,
        "right" => 2,
        other => return Err(Refused(format!("unknown button {other:?}"))),
    })
}

pub async fn click(id: Option<u32>, x: f64, y: f64, button: Option<&str>, count: u8) -> Result<Value, Refused> {
    may_act(false).await?;
    let d = display(id).await?;
    let b = button_code(button)?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny).await?;
    for _ in 0..count.clamp(1, 3) {
        inject([InputEvent::MouseDown { button: b }, InputEvent::MouseUp { button: b }]).await?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let what = match (count, button.unwrap_or("left")) {
        (2, _) => "double-click",
        (3, _) => "triple-click",
        (_, "right") => "right-click",
        _ => "click",
    };
    announce(what, format!("at {x:.0}, {y:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn pointer(id: Option<u32>, x: f64, y: f64) -> Result<Value, Refused> {
    may_act(false).await?;
    let d = display(id).await?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny).await?;
    announce("move", format!("to {x:.0}, {y:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn drag(id: Option<u32>, from: [f64; 2], to: [f64; 2]) -> Result<Value, Refused> {
    may_act(false).await?;
    let d = display(id).await?;
    let (fx, fy) = normalize(&d, from[0], from[1]);
    let (tx, ty) = normalize(&d, to[0], to[1]);
    move_to(&d, fx, fy).await?;
    inject([InputEvent::MouseDown { button: 0 }]).await?;
    // In steps, so apps see a drag rather than a jump.
    const STEPS: u32 = 12;
    for i in 1..=STEPS {
        let t = i as f32 / STEPS as f32;
        tokio::time::sleep(Duration::from_millis(16)).await;
        move_to(&d, fx + (tx - fx) * t, fy + (ty - fy) * t).await?;
    }
    inject([InputEvent::MouseUp { button: 0 }]).await?;
    announce("drag", format!("from {:.0}, {:.0} to {:.0}, {:.0}", from[0], from[1], to[0], to[1]), Some((d.id, tx, ty)));
    Ok(json!({ "ok": true }))
}

pub async fn scroll(id: Option<u32>, x: f64, y: f64, dx: f64, dy: f64) -> Result<Value, Refused> {
    may_act(false).await?;
    let d = display(id).await?;
    let (nx, ny) = normalize(&d, x, y);
    move_to(&d, nx, ny).await?;
    inject([InputEvent::Scroll { dx: dx as f32, dy: dy as f32 }]).await?;
    announce("scroll", format!("by {dx:.0}, {dy:.0}"), Some((d.id, nx, ny)));
    Ok(json!({ "ok": true }))
}

pub async fn type_text(text: &str) -> Result<Value, Refused> {
    may_act(false).await?;
    if text.chars().count() > 10_000 {
        return Err(Refused("too much text at once (10,000 characters at most)".into()));
    }
    crate::grants::check_input(crate::grants::Target::Keys).map_err(Refused)?;
    shared()?.injector.local().map_err(Refused)?.text(text.into()).map_err(Refused)?
        .wait().await.map_err(|e| Refused(e.to_string()))?;
    // An agent's typing isn't shown to viewers or logged, nor anyone's into a password
    // field: it may be a secret.
    let shown = if matches!(crate::grants::current(), crate::grants::Actor::Agent(_)) || crate::grants::secure_input() {
        format!("{} characters", text.chars().count())
    } else {
        let shown: String = text.chars().take(60).collect();
        let more = if text.chars().count() > 60 { "…" } else { "" };
        format!("{shown:?}{more}")
    };
    announce("type", shown, None);
    Ok(json!({ "ok": true }))
}

pub async fn key(keys: &str) -> Result<Value, Refused> {
    may_act(false).await?;
    crate::grants::check_input(crate::grants::Target::Keys).map_err(Refused)?;
    let codes = crate::keys::combo(keys).map_err(Refused)?;
    let mut events: Vec<InputEvent> = codes.iter().map(|&keycode| InputEvent::KeyDown { keycode }).collect();
    events.extend(codes.iter().rev().map(|&keycode| InputEvent::KeyUp { keycode }));
    inject(events).await?;
    announce("key", shown_keys(keys), None);
    Ok(json!({ "ok": true }))
}

/// Keys as viewers and the log see them: not which ones, while a password field has the
/// keyboard (a password typed key by key would be spelled out).
fn shown_keys(keys: &str) -> String {
    if crate::grants::secure_input() { "a key, into a password field".into() } else { keys.to_string() }
}

/// What `wait_for_change` waits for (crate::damage::Wait, as an agent gives it).
pub struct WaitFor {
    /// For a change first (after `since`, or the call).
    pub change: bool,
    /// Then for quiet this long; by default 300 ms, or none after a change.
    pub idle: Option<Duration>,
    /// x, y, width, height in the pixels of the latest screenshot of the display.
    pub region: Option<[f64; 4]>,
    /// A `seq` from a screenshot, an action or an earlier wait.
    pub since: Option<u64>,
    pub timeout: Duration,
}

/// Waits for the display (or part of it) to settle, or to change (or for `timeout`), from
/// what capture says changed: nothing is missed for being small.
pub async fn wait_for_change(id: Option<u32>, w: WaitFor) -> Result<Value, Refused> {
    may_act(true).await?;
    let d = display(id).await?;
    let region = match w.region {
        None => None,
        Some(r) => Some(region_of(&d, r)?),
    };
    follow(d.id).await?;
    let idle = w.idle.unwrap_or(if w.change { Duration::ZERO } else { crate::damage::DEFAULT_IDLE });
    let waited = crate::damage::wait(d.id, crate::damage::Wait { change: w.change, idle, region, since: w.since, timeout: w.timeout }).await;
    Ok(json!({
        "changed": waited.changed,
        "timed_out": waited.timed_out,
        "seq": waited.seq,
        "waited_ms": waited.waited.as_millis() as u64,
    }))
}

/// `[x, y, width, height]` in the pixels of the latest screenshot of `d`, as 0..1 of it.
fn region_of(d: &DisplayInfo, [x, y, w, h]: [f64; 4]) -> Result<crate::damage::Region, Refused> {
    let (sw, sh) = shot_size(d);
    let (sw, sh) = (sw.max(1) as f64, sh.max(1) as f64);
    if !(w > 0.0 && h > 0.0) || [x, y, w, h].iter().any(|v| !v.is_finite()) {
        return Err(Refused("region needs a width and a height".into()));
    }
    if x >= sw || y >= sh || x + w <= 0.0 || y + h <= 0.0 {
        return Err(Refused(format!("region is off the screenshot ({sw}×{sh} pixels)")));
    }
    Ok(crate::damage::Region { x: x / sw, y: y / sh, w: w / sw, h: h / sh })
}

/// Asks the person something and waits for their answer: `help` (they do something, then
/// say done) or `confirm` (yes or no).
/// A question an agent asked, until the agent has its answer: who asked (only they may
/// collect it), and the answer once there is one (`None` inside: nobody answered in time).
struct Waiting {
    kind: &'static str,
    asker: String,
    text: String,
    answer: Option<Option<Answer>>,
    /// Kept this long after the answer comes, for the agent to collect.
    until: Instant,
}

static WAITING: Mutex<Option<HashMap<u64, Waiting>>> = Mutex::new(None);

fn waiting<R>(f: impl FnOnce(&mut HashMap<u64, Waiting>) -> R) -> R {
    let mut w = WAITING.lock().unwrap();
    let map = w.get_or_insert_with(HashMap::new);
    map.retain(|_, x| x.until > Instant::now());
    f(map)
}

/// Who's asking: an agent's grant, or the owner's own tools.
pub(crate) fn asker() -> String {
    match crate::grants::current() {
        crate::grants::Actor::Agent(g) => g.id,
        crate::grants::Actor::Owner => "owner".into(),
    }
}

/// Asks the person and waits for their answer: `help` (they do something, then say done)
/// or `confirm` (yes or no). With `wait` (agents in the cloud, whose requests can't stay
/// open for long), gives up waiting after it and says the question is pending; the
/// question stays up, and `check_answer` collects the answer later.
pub async fn ask(kind: &'static str, text: &str, wait: Option<Duration>) -> Result<Value, Refused> {
    // Asking touches nothing: it doesn't wait for the person to stop using the computer.
    may_act(true).await?;
    let mut text: String = text.chars().take(500).collect();
    if let crate::grants::Actor::Agent(g) = crate::grants::current() {
        text = format!("{} ({}): {text}", g.client_name, g.domain);
    }
    let who = asker();
    // The same question again (an agent retrying): the one already up, not another.
    let (same, open) = waiting(|w| {
        let mine = w.iter().filter(|(_, x)| x.asker == who && x.answer.is_none());
        let same = mine.clone().find(|(_, x)| x.kind == kind && x.text == text).map(|(id, _)| *id);
        (same, mine.count())
    });
    if let Some(id) = same {
        return wait_answer(id, wait.unwrap_or(ASK_WAIT + Duration::from_secs(1))).await;
    }
    if open >= MAX_OPEN_ASKS {
        return Err(Refused(format!("{open} of your questions are still waiting for the person; collect their answers with check_answer first.")));
    }
    let (id, rx) = post_ask(kind, &text, None);
    waiting(|w| w.insert(id, Waiting { kind, asker: who, text: text.clone(), answer: None, until: Instant::now() + ASK_WAIT + KEEP_ANSWER }));
    tokio::spawn(async move {
        let answer = collect(id, rx, ASK_WAIT).await;
        waiting(|w| {
            if let Some(x) = w.get_mut(&id) {
                x.answer = Some(answer);
                x.until = Instant::now() + KEEP_ANSWER;
            }
        });
    });
    wait_answer(id, wait.unwrap_or(ASK_WAIT + Duration::from_secs(1))).await
}

/// Questions one agent may have waiting at once.
const MAX_OPEN_ASKS: usize = 3;

/// How long an answer waits for the agent to collect it.
const KEEP_ANSWER: Duration = Duration::from_secs(3600);

/// The answer to an earlier question (`check_answer`), waiting up to `wait` for it.
pub async fn check_answer(id: u64, wait: Duration) -> Result<Value, Refused> {
    let mine = waiting(|w| w.get(&id).map(|x| x.asker == asker()));
    match mine {
        Some(true) => wait_answer(id, wait).await,
        _ => Err(Refused(format!("No question {id} of yours is waiting; ask again if you still need to."))),
    }
}

async fn wait_answer(id: u64, wait: Duration) -> Result<Value, Refused> {
    let start = Instant::now();
    loop {
        let state = waiting(|w| w.get(&id).map(|x| (x.kind, x.answer.clone())));
        match state {
            Some((kind, Some(answer))) => {
                waiting(|w| w.remove(&id));
                let answer = answer.ok_or_else(|| Refused("Nobody answered.".into()))?;
                return Ok(match kind {
                    "confirm" => json!({ "kind": kind, "approved": answer.ok, "note": answer.note }),
                    _ => json!({ "kind": kind, "done": answer.ok, "note": answer.note }),
                });
            }
            Some((kind, None)) if start.elapsed() >= wait => {
                return Ok(json!({ "kind": kind, "pending": true, "id": id }));
            }
            Some(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            None => return Err(Refused(format!("No question {id} is waiting."))),
        }
    }
}

/// Asks the owner (in viewers, the Glass app and a notification) and waits for the answer;
/// `None` if nobody answers in time. `consent` asks show `choices` to pick from.
pub async fn ask_owner(kind: &'static str, text: &str, choices: Option<Vec<String>>, wait: Duration) -> Option<Answer> {
    let (id, rx) = post_ask(kind, text, choices);
    collect(id, rx, wait).await
}

/// The answer, or `None` (and the question taken down) after `wait`.
async fn collect(id: u64, rx: oneshot::Receiver<Answer>, wait: Duration) -> Option<Answer> {
    match tokio::time::timeout(wait, rx).await {
        Ok(Ok(a)) => Some(a),
        _ => {
            with(|s| s.asks.remove(&id));
            broadcast(ServerMsg::AgentAnswered { id });
            None
        }
    }
}

/// Puts a question up for the owner: viewers, the Glass app, a notification.
fn post_ask(kind: &'static str, text: &str, choices: Option<Vec<String>>) -> (u64, oneshot::Receiver<Answer>) {
    let (tx, rx) = oneshot::channel();
    let id = with(|s| {
        let id = new_ask_id();
        s.asks.insert(id, Ask { kind, text: text.to_string(), choices: choices.clone(), reply: tx });
        id
    });
    info!("agent: asks ({kind}) {text:?}");
    crate::testmode::record("agent", json!({ "ask": kind, "text": text, "choices": choices }));
    broadcast(ServerMsg::AgentAsk { id, kind, text: text.to_string(), choices });
    let title = match kind {
        "help" => "An agent needs your help",
        "consent" => "An agent asks to use this computer",
        _ => "An agent asks before it acts",
    };
    if kind == "consent" {
        // The request id isn't for people; where to answer is.
        let shown = text.rsplit_once(" [").map_or(text, |(t, _)| t);
        crate::notify::notify(title, &format!("{shown} Answer in Glass › AI agents, or in a viewer."));
    } else {
        crate::notify::notify(title, text);
    }
    crate::push::agent_ask(id, kind, text);
    (id, rx)
}

/// Withdraws a question nobody needs answered any more.
pub fn withdraw(id_text: &str) {
    let ids: Vec<u64> = with(|s| s.asks.iter().filter(|(_, a)| a.text == id_text).map(|(id, _)| *id).collect());
    for id in ids {
        with(|s| s.asks.remove(&id));
        broadcast(ServerMsg::AgentAnswered { id });
    }
}

/// One action by an agent under a grant: allowed by its scopes and budget, run with the
/// grant in force (so input is checked against its apps), and written to the audit log.
pub async fn act_as(grant: &crate::grants::Grant, action: Action, charge: bool) -> Result<Value, Refused> {
    let (name, view_only) = action.describe();
    let needed = if view_only { crate::grants::VIEW } else { crate::grants::USE };
    if !grant.has(needed) {
        crate::grants::audit(grant, name, "refused: not allowed by its grant", None, false);
        return Err(Refused(format!("This agent isn't allowed to {name} (it needs {needed}).")));
    }
    match if charge { crate::grants::spend(grant) } else { crate::grants::Spend::Ok } {
        crate::grants::Spend::Ok => {}
        over => {
            if matches!(over, crate::grants::Spend::JustOver) {
                crate::grants::audit(grant, name, "refused: hourly limit reached", None, false);
                crate::push::activity(
                    &format!("{} reached its hourly limit", grant.client_name),
                    &format!("It's held until the hour ends. Resume agents in Glass to let it continue now ({} actions an hour).", crate::grants::HOURLY_BUDGET),
                    "agent-budget",
                );
            }
            return Err(Refused(format!(
                "This agent used its {} actions for this hour. It can continue next hour, or when the owner resumes agents in Glass.",
                crate::grants::HOURLY_BUDGET
            )));
        }
    }
    AGENT_AT.store(epoch_ms(), std::sync::atomic::Ordering::Relaxed);
    let detail = action.audit_detail();
    // The app in front when it acted (after, a click may have switched apps).
    let app = crate::grants::frontmost_app();
    let actor = crate::grants::Actor::Agent(grant.clone());
    let result = crate::grants::ACTOR.scope(actor, act(action)).await;
    AGENT_AT.store(epoch_ms(), std::sync::atomic::Ordering::Relaxed);
    match &result {
        Ok(_) => crate::grants::audit(grant, name, &detail, app.as_deref(), true),
        Err(Refused(why)) => crate::grants::audit(grant, name, &format!("{detail} — refused: {why}"), app.as_deref(), false),
    }
    result
}

// ---- HTTP ----

pub fn routes() -> axum::Router {
    axum::Router::new()
        .route("/agent/v1/displays", get(http_displays))
        .route("/agent/v1/status", get(http_status))
        .route("/agent/v1/act", post(http_act))
}

type Peer = ConnectInfo<SocketAddr>;

fn reply(result: Result<Value, Refused>) -> Response<Body> {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(Refused(msg)) => (StatusCode::CONFLICT, Json(json!({ "error": msg }))).into_response(),
    }
}

async fn owner(peer: &Peer, headers: &HeaderMap) -> Option<Response<Body>> {
    (!crate::forward::is_owner(peer.0.ip(), headers).await)
        .then(|| (StatusCode::FORBIDDEN, Json(json!({ "error": "owner only" }))).into_response())
}

/// The displays, for an agent under a grant: scoped, counted and logged like any action.
pub async fn displays_as(grant: &crate::grants::Grant) -> Result<Value, Refused> {
    if !grant.has(crate::grants::VIEW) {
        return Err(Refused("This agent isn't allowed to see the screen.".into()));
    }
    if !matches!(crate::grants::spend(grant), crate::grants::Spend::Ok) {
        return Err(Refused(format!("This agent used its {} actions for this hour.", crate::grants::HOURLY_BUDGET)));
    }
    let v = displays_json().await;
    crate::grants::audit(grant, "list_displays", "", None, v.is_ok());
    v
}

/// The displays, as agents see them.
pub async fn displays_json() -> Result<Value, Refused> {
    Ok(json!(displays().await?.iter().map(|d| json!({
        "id": d.id, "width": d.width, "height": d.height, "main": d.main,
        "x": d.x, "y": d.y,
    })).collect::<Vec<_>>()))
}

async fn http_displays(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    reply(displays_json().await.map(|all| json!({ "displays": all })))
}

async fn http_status(peer: Peer, headers: HeaderMap) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    Json(json!({ "status": with(|s| s.status) })).into_response()
}

/// One action: `{"action": "click", "x": …, "y": …, "display": …}` and so on.
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Screenshot { display: Option<u32>, max_width: Option<u32>, max_height: Option<u32> },
    Click { display: Option<u32>, x: f64, y: f64, button: Option<String>, #[serde(default)] count: Option<u8> },
    Move { display: Option<u32>, x: f64, y: f64 },
    Drag { display: Option<u32>, from_x: f64, from_y: f64, to_x: f64, to_y: f64 },
    Scroll { display: Option<u32>, x: f64, y: f64, #[serde(default)] dx: f64, #[serde(default)] dy: f64 },
    Type { text: String },
    Key { keys: String },
    WaitForChange {
        display: Option<u32>,
        timeout_ms: Option<u64>,
        idle_ms: Option<u64>,
        #[serde(default)]
        change: bool,
        /// x, y, width, height in screenshot pixels.
        region: Option<[f64; 4]>,
        since: Option<u64>,
    },
    Snapshot { app: Option<String>, #[serde(default)] all_windows: bool, max_lines: Option<usize> },
    Press { #[serde(rename = "ref")] target: String },
    SetValue { #[serde(rename = "ref")] target: String, text: String },
    RequestHelp { text: String, wait_ms: Option<u64> },
    Confirm { text: String, wait_ms: Option<u64> },
    CheckAnswer { id: u64, wait_ms: Option<u64> },
    /// Records the screen for `seconds`, or starts a recording (`start`) or stops one
    /// (`stop`, its id).
    Record { seconds: Option<f64>, #[serde(default)] start: bool, stop: Option<u64> },
}

impl Action {
    /// Its name, and whether it only looks.
    pub fn describe(&self) -> (&'static str, bool) {
        match self {
            Action::Screenshot { .. } => ("screenshot", true),
            Action::WaitForChange { .. } => ("wait_for_change", true),
            Action::Snapshot { .. } => ("snapshot", true),
            Action::Press { .. } => ("press", false),
            Action::SetValue { .. } => ("set_value", false),
            Action::Click { .. } => ("click", false),
            Action::Move { .. } => ("move", false),
            Action::Drag { .. } => ("drag", false),
            Action::Scroll { .. } => ("scroll", false),
            Action::Type { .. } => ("type", false),
            Action::Key { .. } => ("key", false),
            Action::RequestHelp { .. } => ("request_help", false),
            Action::Confirm { .. } => ("confirm", false),
            Action::CheckAnswer { .. } => ("check_answer", false),
            Action::Record { .. } => ("record", true),
        }
    }

    /// What the audit log keeps (never the text typed).
    fn audit_detail(&self) -> String {
        match self {
            Action::Screenshot { display, .. } | Action::WaitForChange { display, .. } => {
                display.map(|d| format!("display {d}")).unwrap_or_default()
            }
            Action::Click { x, y, button, count, .. } => {
                format!("{} ×{} at {x:.0}, {y:.0}", button.as_deref().unwrap_or("left"), count.unwrap_or(1))
            }
            Action::Move { x, y, .. } | Action::Scroll { x, y, .. } => format!("at {x:.0}, {y:.0}"),
            Action::Drag { from_x, from_y, to_x, to_y, .. } => format!("{from_x:.0}, {from_y:.0} → {to_x:.0}, {to_y:.0}"),
            Action::Type { text } => format!("{} characters", text.chars().count()),
            Action::Snapshot { app, .. } => app.clone().unwrap_or_default(),
            Action::Press { target } => target.clone(),
            Action::SetValue { target, text } => format!("{target}: {} characters", text.chars().count()),
            Action::Key { keys } => shown_keys(keys),
            Action::RequestHelp { text, .. } | Action::Confirm { text, .. } => text.chars().take(200).collect(),
            Action::CheckAnswer { id, .. } => format!("question {id}"),
            Action::Record { seconds, start, stop } => match (stop, start) {
                (Some(id), _) => format!("stop {id}"),
                (None, true) => "start".into(),
                (None, false) => format!("{} s", seconds.unwrap_or(5.0)),
            },
        }
    }
}

pub async fn act(action: Action) -> Result<Value, Refused> {
    // What the screen shows from here on is this action's doing: a wait after it can count
    // changes since its `seq`.
    let input = !action.describe().1 && !matches!(action, Action::RequestHelp { .. } | Action::Confirm { .. } | Action::CheckAnswer { .. });
    let seq = crate::damage::seq();
    let mut v = run(action).await?;
    if input && let Some(o) = v.as_object_mut() {
        o.insert("seq".into(), json!(seq));
    }
    Ok(v)
}

async fn run(action: Action) -> Result<Value, Refused> {
    match action {
        Action::Screenshot { display, max_width, max_height } => {
            let max = (max_width.is_some() || max_height.is_some())
                .then(|| [max_width.unwrap_or(LARGEST), max_height.unwrap_or(LARGEST)]);
            screenshot(display, max).await
        }
        Action::Click { display, x, y, button, count } => {
            click(display, x, y, button.as_deref(), count.unwrap_or(1)).await
        }
        Action::Move { display, x, y } => pointer(display, x, y).await,
        Action::Drag { display, from_x, from_y, to_x, to_y } => drag(display, [from_x, from_y], [to_x, to_y]).await,
        Action::Scroll { display, x, y, dx, dy } => scroll(display, x, y, dx, dy).await,
        Action::Type { text } => type_text(&text).await,
        Action::Key { keys } => key(&keys).await,
        Action::WaitForChange { display, timeout_ms, idle_ms, change, region, since } => {
            let w = WaitFor {
                change,
                idle: idle_ms.map(|ms| Duration::from_millis(ms.min(60_000))),
                region,
                since,
                timeout: Duration::from_millis(timeout_ms.unwrap_or(10_000).min(60_000)),
            };
            wait_for_change(display, w).await
        }
        Action::Snapshot { app, all_windows, max_lines } => crate::elements::snapshot(app, all_windows, max_lines).await,
        Action::Press { target } => crate::elements::press(&target).await,
        Action::SetValue { target, text } => crate::elements::set_value(&target, &text).await,
        Action::RequestHelp { text, wait_ms } => ask("help", &text, wait_ms.map(Duration::from_millis)).await,
        Action::Confirm { text, wait_ms } => ask("confirm", &text, wait_ms.map(Duration::from_millis)).await,
        Action::CheckAnswer { id, wait_ms } => {
            check_answer(id, Duration::from_millis(wait_ms.unwrap_or(0).min(ASK_WAIT.as_millis() as u64))).await
        }
        Action::Record { seconds, start, stop } => match (stop, start) {
            (Some(id), _) => crate::clips::stop(id).await,
            (None, true) => crate::clips::start().await,
            (None, false) => crate::clips::record(crate::clips::record_len(seconds.unwrap_or(5.0))?).await,
        },
    }
}

async fn http_act(peer: Peer, headers: HeaderMap, body: axum::body::Bytes) -> Response<Body> {
    if let Some(r) = owner(&peer, &headers).await {
        return r;
    }
    // Never a page elsewhere posting text/plain without asking (no preflight).
    if let Some(r) = crate::origin::require_json(&headers) {
        return r;
    }
    match serde_json::from_slice::<Action>(&body) {
        Ok(action) => reply(act(action).await),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(w: usize, h: usize) -> DisplayInfo {
        DisplayInfo { id: 99, width: w, height: h, x: 0.0, y: 0.0, points_width: w as f64, points_height: h as f64, main: true, builtin: false }
    }

    #[test]
    fn coordinates_are_in_the_default_screenshot_until_one_is_taken() {
        let d = display(2560, 1600);
        // Default screenshot: 1280×800.
        assert_eq!(normalize(&d, 640.0, 400.0), (0.5, 0.5));
        with(|s| s.sizes.insert(("owner".into(), 99), (100, 50)));
        assert_eq!(normalize(&d, 25.0, 50.0), (0.25, 1.0));
        with(|s| s.sizes.remove(&("owner".into(), 99)));
    }
}
