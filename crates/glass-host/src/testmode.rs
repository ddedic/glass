//! `glass --test`: deterministic fakes for end-to-end tests. A generated test pattern on
//! two fake displays replaces screen capture, input is recorded instead of injected, the
//! clipboard and file chooser are in memory, and HTTP hooks inject faults. Nothing here
//! touches the real screen, mouse, keyboard or clipboard.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::Json;
use axum::extract::Query;
use axum::routing::{get, post};
use glass_proto::{
    AUDIO_FRAMES_PER_PACKET, AUDIO_SAMPLE_RATE, InputEvent, MAX_STREAMS, audio_packet,
};
use tokio::sync::broadcast;

use crate::clock::now_us;
use crate::control::ServerMsg;
use crate::media::{
    AudioPackets, Clipboard, DisplayInfo, EncodedFrame, InputSink, Layout, VideoSource,
};
use crate::soft_encode::{Encoder, Frame, PixelFormat};

const WIDTH: usize = 640;
const HEIGHT: usize = 360;
const DISPLAYS: usize = 2;
/// Fake windows for single-app streaming: (id, app, title), each WINDOW_WIDTH×WINDOW_HEIGHT.
const WINDOWS: [(u32, &str, &str); 2] = [(100, "Notes", "Notes — todo.txt"), (101, "Terminal", "Terminal")];
const WINDOW_WIDTH: usize = 480;
const WINDOW_HEIGHT: usize = 320;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Frames to drop before sending (simulated loss).
static DROP_FRAMES: AtomicU32 = AtomicU32::new(0);
/// Corrupt the next frame after its checksum is computed.
static CORRUPT_NEXT: AtomicBool = AtomicBool::new(false);
/// Sessions ignore what viewers send as datagrams (pings included): a page gone quiet.
static DEAF: AtomicBool = AtomicBool::new(false);
/// Self-update requests, readable at /test/updates.
static UPDATES: AtomicU32 = AtomicU32::new(0);

pub fn record_update() {
    UPDATES.fetch_add(1, Ordering::Relaxed);
}

/// "Fit this device" requests, in order (test mode never touches real displays).
static FITS: Mutex<Vec<Option<[u32; 2]>>> = Mutex::new(Vec::new());

pub fn record_fit(size: Option<[u32; 2]>) {
    FITS.lock().unwrap().push(size);
}

/// Everything else the host did instead of touching the real computer, by kind
/// (notification, privacy, restart, mic, wake), readable at /test/records?kind=….
static RECORDS: Mutex<Vec<(&'static str, serde_json::Value)>> = Mutex::new(Vec::new());

pub fn record(kind: &'static str, value: serde_json::Value) {
    RECORDS.lock().unwrap().push((kind, value));
}

pub fn record_notification(title: &str, body: &str) {
    record("notification", serde_json::json!({"title": title, "body": body}));
}

pub fn record_privacy(on: bool) {
    record("privacy", serde_json::json!(on));
}

pub fn record_restart(what: &str) {
    record("restart", serde_json::json!(what));
}

pub fn record_mic(bytes: usize) {
    record("mic", serde_json::json!(bytes));
}

pub fn record_wake(mac: &str) {
    record("wake", serde_json::json!(mac));
}

/// Current adaptive bitrate, readable at /test/bitrate.
static BITRATE: AtomicU32 = AtomicU32::new(0);
/// Keyframes encoded and the bytes of the biggest, also there (for the bench).
static KEYFRAMES: AtomicU32 = AtomicU32::new(0);
static BIGGEST_KEYFRAME: AtomicU32 = AtomicU32::new(0);
/// Refreshes real capture sends after a change: 12 a second, stopping after 3 tiny ones.
const STILL_REFRESH_EVERY: Duration = Duration::from_millis(83);
const STILL_TINY: usize = 1024;

pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The OS test mode reports to clients: `GLASS_TEST_OS` (macos or linux), default macos,
/// so client behaviour under test doesn't depend on the machine running the tests.
pub fn host_os() -> &'static str {
    match std::env::var("GLASS_TEST_OS").as_deref() {
        Ok("linux") => "linux",
        _ => "macos",
    }
}

/// Server hook: true if this frame should be dropped (its id still advances).
pub fn take_drop() -> bool {
    if !enabled() {
        return false;
    }
    let mut n = DROP_FRAMES.load(Ordering::Relaxed);
    while n > 0 {
        match DROP_FRAMES.compare_exchange_weak(n, n - 1, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(current) => n = current,
        }
    }
    false
}

/// Server hook: true if this frame should be corrupted in flight.
pub fn deaf() -> bool {
    enabled() && DEAF.load(Ordering::Relaxed)
}

pub fn take_corrupt() -> bool {
    enabled() && CORRUPT_NEXT.swap(false, Ordering::Relaxed)
}

// ---- Video: a moving test pattern per fake display ----

/// The pattern stops moving (as if the screen went still), for lossless-refresh tests.
static PAUSED: AtomicBool = AtomicBool::new(false);
/// Sharp text asked for full-colour pictures: keep the latest one per source.
static FULL_COLOUR: AtomicBool = AtomicBool::new(false);
type Latest = Mutex<Vec<(u64, Option<Arc<crate::lossless::RawFrame>>)>>;
static LATEST: Latest = Mutex::new(Vec::new());

pub struct TestVideo {
    /// The fake displays, then the fake windows (as streams they look the same).
    sources: Vec<DisplayInfo>,
    slots: Vec<Arc<AtomicI32>>,
    keyframes: Vec<Arc<AtomicBool>>,
    shown: Mutex<Vec<u32>>,
    /// Per display: the width to encode at (dynamic resolution).
    widths: Vec<Arc<std::sync::atomic::AtomicUsize>>,
    /// `--fps`: the test displays' refresh rate.
    refresh: u32,
}

/// The test pattern's frame rate now: `--fps`, or less if viewers want fewer.
static PATTERN_FPS: AtomicU32 = AtomicU32::new(30);

impl TestVideo {
    pub fn new(frames: broadcast::Sender<Arc<EncodedFrame>>, fps: u32, bitrate: u32) -> Self {
        BITRATE.store(bitrate, Ordering::Relaxed);
        PATTERN_FPS.store(fps.min(crate::refresh::DEFAULT), Ordering::Relaxed);
        let (width, height) = display_size();
        let displays: Vec<DisplayInfo> = (0..DISPLAYS)
            .map(|i| DisplayInfo {
                id: i as u32,
                width,
                height,
                x: (i * width) as f64,
                y: 0.0,
                points_width: width as f64,
                points_height: height as f64,
                main: i == 0,
                builtin: false,
            })
            .chain(WINDOWS.iter().enumerate().map(|(i, &(id, _, _))| DisplayInfo {
                id,
                width: WINDOW_WIDTH,
                height: WINDOW_HEIGHT,
                x: 40.0 + i as f64 * 60.0,
                y: 40.0,
                points_width: WINDOW_WIDTH as f64,
                points_height: WINDOW_HEIGHT as f64,
                main: false,
                builtin: false,
            }))
            .collect();
        let count = displays.len();
        let slots: Vec<_> = (0..count)
            .map(|_| Arc::new(AtomicI32::new(-1)))
            .collect();
        let keyframes: Vec<_> = (0..count)
            .map(|_| Arc::new(AtomicBool::new(true)))
            .collect();
        let widths: Vec<_> = displays
            .iter()
            .map(|d| Arc::new(std::sync::atomic::AtomicUsize::new(d.width)))
            .collect();
        LATEST.lock().unwrap().resize(count, (0, None));
        for i in 0..count {
            let (slot, keyframe, frames) = (slots[i].clone(), keyframes[i].clone(), frames.clone());
            let width = widths[i].clone();
            let size = (displays[i].width, displays[i].height);
            std::thread::spawn(move || pattern_loop(i, size, slot, keyframe, width, frames));
        }
        Self {
            sources: displays,
            slots,
            keyframes,
            shown: Mutex::new(Vec::new()),
            widths,
            refresh: fps,
        }
    }
}

impl TestVideo {
    /// The source currently carried by `stream`.
    fn source_of(&self, stream: u8) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.load(Ordering::Relaxed) == stream as i32)
    }
}

impl VideoSource for TestVideo {
    fn supports_lossless(&self) -> bool {
        true
    }

    fn set_max_fps(&self, fps: u32) {
        PATTERN_FPS.store(self.refresh.min(fps).max(1), Ordering::Relaxed);
    }

    fn set_full_colour(&self, on: bool) {
        // Real capture restarts for this (BGRA), and a new encoder starts on a keyframe.
        if FULL_COLOUR.swap(on, Ordering::Relaxed) != on && bench().is_some() {
            for k in &self.keyframes {
                k.store(true, Ordering::Relaxed);
            }
        }
    }

    fn picture(&self, id: u32, max: [u32; 2]) -> Option<crate::lossless::RawFrame> {
        let index = self.sources[..DISPLAYS].iter().position(|d| d.id == id)?;
        let d = &self.sources[index];
        let (w, h) = crate::thumbs::fit(d.width as usize, d.height as usize, max);
        let frame = render(index, w, h, 0);
        Some(crate::lossless::RawFrame { width: w, height: h, bgra: frame.data, capture_us: 0 })
    }

    fn frame_generation(&self, stream: u8) -> Option<u64> {
        let d = self.source_of(stream)?;
        LATEST.lock().unwrap().get(d).map(|(g, _)| *g)
    }

    fn raw_frame(&self, stream: u8) -> Option<Arc<crate::lossless::RawFrame>> {
        let d = self.source_of(stream)?;
        LATEST.lock().unwrap().get(d).and_then(|(_, f)| f.clone())
    }

    fn streams(&self) -> Vec<DisplayInfo> {
        self.shown
            .lock()
            .unwrap()
            .iter()
            .map(|&i| self.sources[i as usize].clone())
            .collect()
    }

    fn set_target_widths(&self, widths: &[Option<u32>]) {
        for (d, slot) in self.slots.iter().enumerate() {
            let s = slot.load(Ordering::Relaxed);
            if s >= 0 {
                let native = self.sources[d].width;
                let w = widths
                    .get(s as usize)
                    .copied()
                    .flatten()
                    .map_or(native, |w| (w as usize).min(native));
                self.widths[d].store(w, Ordering::Relaxed);
            }
        }
    }

    fn request_keyframe(&self, stream: Option<u8>) {
        for (i, slot) in self.slots.iter().enumerate() {
            let s = slot.load(Ordering::Relaxed);
            if s >= 0 && stream.is_none_or(|n| n as i32 == s) {
                self.keyframes[i].store(true, Ordering::Relaxed);
            }
        }
    }

    fn set_layout(&self, layout: Layout) -> Result<Vec<DisplayInfo>> {
        let ids: Vec<u32> = match layout {
            Layout::Single(Some(id)) => {
                anyhow::ensure!((id as usize) < DISPLAYS, "unknown display");
                vec![id]
            }
            Layout::Single(None) => vec![0],
            Layout::Window(id) => {
                let i = self
                    .sources
                    .iter()
                    .position(|d| d.id == id && d.id >= WINDOWS[0].0)
                    .ok_or_else(|| anyhow::anyhow!("that window is gone"))?;
                vec![i as u32]
            }
            Layout::All => (0..DISPLAYS.min(MAX_STREAMS) as u32).collect(),
        };
        for slot in &self.slots {
            slot.store(-1, Ordering::Relaxed);
        }
        for (n, &id) in ids.iter().enumerate() {
            self.slots[id as usize].store(n as i32, Ordering::Relaxed);
            self.keyframes[id as usize].store(true, Ordering::Relaxed);
        }
        *self.shown.lock().unwrap() = ids.clone();
        Ok(ids
            .iter()
            .map(|&i| self.sources[i as usize].clone())
            .collect())
    }

    fn displays(&self) -> Result<Vec<DisplayInfo>> {
        Ok(self.sources[..DISPLAYS].to_vec())
    }

    fn supports_windows(&self) -> bool {
        true
    }

    fn windows(&self) -> Result<Vec<crate::media::WindowInfo>> {
        Ok(WINDOWS
            .iter()
            .zip(&self.sources[DISPLAYS..])
            .map(|(&(id, app, title), d)| crate::media::WindowInfo {
                id,
                app: app.into(),
                title: title.into(),
                width: d.width,
                height: d.height,
                x: d.x,
                y: d.y,
                points_width: d.points_width,
                points_height: d.points_height,
            })
            .collect())
    }

    fn set_bitrate(&self, bps: u32) {
        BITRATE.store(bps, Ordering::Relaxed);
    }
}

fn pattern_loop(
    display: usize,
    (w, h): (usize, usize),
    slot: Arc<AtomicI32>,
    keyframe: Arc<AtomicBool>,
    width: Arc<std::sync::atomic::AtomicUsize>,
    frames: broadcast::Sender<Arc<EncodedFrame>>,
) {
    let mut applied = BITRATE.load(Ordering::Relaxed);
    let mut encoded_width = w;
    let mut fps = PATTERN_FPS.load(Ordering::Relaxed);
    let mut encoder = Encoder::new(w, h, fps, applied, 1.0).expect("test encoder");
    let mut next = Instant::now();
    // The pattern's position: frozen while paused, so frames repeat the same picture.
    let mut shown = 0u64;
    let mut last_encoded = None;
    // Still workload: tiny refreshes so far, and when the last frame went (None: settled).
    let mut still = (0, None::<Instant>);
    loop {
        next += Duration::from_secs_f64(1.0 / fps as f64);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
        let s = slot.load(Ordering::Relaxed);
        if s < 0 {
            continue;
        }
        if bench() == Some(Bench::Still) && !keyframe.load(Ordering::Relaxed) {
            // Nothing changes: only a refresh that's due.
            if still.1.is_none_or(|at: Instant| at.elapsed() < STILL_REFRESH_EVERY) {
                continue;
            }
        }
        // Dynamic resolution or a new frame rate: a new encoder, starting on a keyframe.
        let want = width.load(Ordering::Relaxed);
        let want_fps = PATTERN_FPS.load(Ordering::Relaxed);
        if want != encoded_width || want_fps != fps {
            fps = want_fps;
            let scale = want as f64 / w as f64;
            if let Ok(e) = Encoder::new(w, h, fps, applied, scale) {
                encoder = e;
                encoded_width = want;
                keyframe.store(true, Ordering::Relaxed);
            }
        }
        let target = BITRATE.load(Ordering::Relaxed);
        if target != applied {
            encoder.set_bitrate(target);
            applied = target;
        }
        if !PAUSED.load(Ordering::Relaxed) {
            shown += 1;
        }
        let frame = render(display, w, h, shown);
        let capture_us = now_us();
        if FULL_COLOUR.load(Ordering::Relaxed) {
            let raw = crate::lossless::RawFrame {
                width: frame.width,
                height: frame.height,
                bgra: frame.data.clone(),
                capture_us,
            };
            if let Some(slot) = LATEST.lock().unwrap().get_mut(display) {
                *slot = (slot.0 + 1, Some(Arc::new(raw)));
            }
        }
        let force = keyframe.swap(false, Ordering::Relaxed);
        if let Ok(Some((data, key))) = encoder.encode(&frame, force) {
            if key {
                KEYFRAMES.fetch_add(1, Ordering::Relaxed);
                BIGGEST_KEYFRAME.fetch_max(data.len() as u32, Ordering::Relaxed);
                still = (0, Some(Instant::now()));
            } else if still.1.is_some() {
                still.0 += (data.len() < STILL_TINY) as u32;
                still.1 = (still.0 < 3).then(Instant::now);
            }
            let refresh = last_encoded == Some(shown);
            last_encoded = Some(shown);
            let _ = frames.send(Arc::new(EncodedFrame {
                stream: s as u8,
                data,
                keyframe: key,
                capture_us,
                hevc: false,
                refresh,
            }));
        }
    }
}

/// A distinct background per display (and window) with a bar sweeping across.
fn render(display: usize, width: usize, height: usize, n: u64) -> Frame {
    if let Some(workload) = bench()
        && display < DISPLAYS
    {
        return bench_render(workload, width, height, n);
    }
    let mut data = vec![0u8; width * height * 4];
    let bar = (n as usize * 8) % width;
    let bg = match display {
        0 => [200, 80, 30],
        1 => [30, 160, 90],
        2 => [170, 60, 160],
        _ => [60, 60, 200],
    };
    for y in 0..height {
        for x in 0..width {
            let p = &mut data[(y * width + x) * 4..][..4];
            let on_bar = x >= bar && x < bar + 24;
            let [b, g, r] = if on_bar { [255, 255, 255] } else { bg };
            p.copy_from_slice(&[b, g, r, 255]);
        }
    }
    Frame {
        width,
        height,
        stride: width * 4,
        format: PixelFormat::Bgrx,
        data,
    }
}

// ---- Bench workloads (bench/): `GLASS_TEST_BENCH=anim|scroll` ----
//
// The displays draw a workload with a marker in the top-left corner that encodes a frame
// counter and the host clock in ms, which the viewer's bench probe (web/bench.js) reads
// back from every decoded frame. Adapted from voxl (MIT, github.com/tembo/voxl): its
// bench/loadgen.c marker and workloads.

#[derive(Clone, Copy, PartialEq, Debug)]
enum Bench {
    /// A box moving and changing colour over a still page of text: part of the screen moves.
    Anim,
    /// A page of text scrolling by 6 px a frame: all of it moves.
    Scroll,
    /// A page of text that doesn't change, as a desktop mostly is. Like real capture it
    /// sends nothing while still: a keyframe when asked and a few refreshes after it, so a
    /// keyframe that doesn't make it isn't followed by frames that reveal the loss.
    Still,
    /// Blocks of noise that change every frame: more than any bitrate can carry, so the
    /// encoder presses against its target and the adaptive rate has to find the link's.
    Noise,
}

fn bench() -> Option<Bench> {
    static WORKLOAD: std::sync::OnceLock<Option<Bench>> = std::sync::OnceLock::new();
    *WORKLOAD.get_or_init(|| match std::env::var("GLASS_TEST_BENCH").as_deref() {
        Ok("anim") => Some(Bench::Anim),
        Ok("scroll") => Some(Bench::Scroll),
        Ok("still") => Some(Bench::Still),
        Ok("noise") => Some(Bench::Noise),
        _ => None,
    })
}

/// The fake displays' size: `GLASS_TEST_SIZE=WxH` (even, at least 128×128), else 640×360.
fn display_size() -> (usize, usize) {
    std::env::var("GLASS_TEST_SIZE")
        .ok()
        .and_then(|s| parse_size(&s))
        .unwrap_or((WIDTH, HEIGHT))
}

fn parse_size(s: &str) -> Option<(usize, usize)> {
    let (w, h) = s.split_once('x')?;
    let (w, h) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    (w >= 128 && h >= 128 && w % 2 == 0 && h % 2 == 0 && w <= 7680 && h <= 4320).then_some((w, h))
}

/// The marker: MARKER_COLS × MARKER_ROWS cells, one bit each (white is 1), each cell
/// 1/MARKER_SCALE of the frame's width and inset by one cell, so the viewer finds it at
/// any decoded size. web/bench.js reads it; keep the two in step.
const MARKER_COLS: usize = 16;
const MARKER_ROWS: usize = 3;
const MARKER_SCALE: usize = 64;

/// 16 bits of frame counter, 24 bits of host clock (ms), 8 bits of check. The check is
/// inverted so an all-black (or all-white) corner never reads as a valid marker.
fn marker_bits(frame: u64, ms: u64) -> u64 {
    let (frame, ms) = (frame & 0xffff, ms & 0xff_ffff);
    let check = ((frame ^ (frame >> 8) ^ ms ^ (ms >> 8) ^ (ms >> 16)) & 0xff) ^ 0xa5;
    frame | (ms << 16) | (check << 40)
}

/// A page of text-like lines, `pages` screens tall (built once per size).
fn page_texture(width: usize, height: usize) -> Arc<Vec<u8>> {
    type Cache = Mutex<Option<((usize, usize), Arc<Vec<u8>>)>>;
    static PAGE: Cache = Mutex::new(None);
    let mut cache = PAGE.lock().unwrap();
    if let Some((size, tex)) = cache.as_ref()
        && *size == (width, height)
    {
        return tex.clone();
    }
    let rows = height * 4;
    let mut tex = vec![0u8; width * rows * 4];
    for y in 0..rows {
        let (line, in_line) = (y / 24, y % 24);
        let len = 40 + (line * 7919) % width.saturating_sub(200).max(1);
        for x in 0..width {
            let mut c = [0xf5, 0xf5, 0xf5];
            if (5..18).contains(&in_line) && x > 40 && x < len {
                let glyph = (x / 9 + line * 31) % 7;
                if glyph != 0 && x % 9 < 7 && (x * 13 + y * 7 + line) % 5 < 3 {
                    c = [0x20, 0x20, 0x20 + (line % 3) as u8 * 0x20];
                }
            }
            if line % 17 == 0 {
                c = [0xd0, 0x70, 0x30];
            }
            tex[(y * width + x) * 4..][..4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    let tex = Arc::new(tex);
    *cache = Some(((width, height), tex.clone()));
    tex
}

fn bench_render(workload: Bench, width: usize, height: usize, n: u64) -> Frame {
    let ms = now_us() / 1000;
    let tex = page_texture(width, height);
    let rows = tex.len() / (width * 4);
    let row_bytes = width * 4;
    let mut data = vec![0u8; width * height * 4];
    let scroll = if workload == Bench::Scroll { (n as usize * 6) % rows } else { 0 };
    for y in 0..height {
        let src = (y + scroll) % rows;
        data[y * row_bytes..][..row_bytes].copy_from_slice(&tex[src * row_bytes..][..row_bytes]);
    }
    if workload == Bench::Noise {
        const BLOCK: usize = 16;
        for by in (0..height).step_by(BLOCK) {
            for bx in (0..width).step_by(BLOCK) {
                let mut v = (bx as u64 * 73_856_093) ^ (by as u64 * 19_349_663) ^ (n * 83_492_791);
                v = v.wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let c = [(v >> 56) as u8, (v >> 48) as u8, (v >> 40) as u8, 255];
                for y in by..(by + BLOCK).min(height) {
                    for x in bx..(bx + BLOCK).min(width) {
                        data[(y * width + x) * 4..][..4].copy_from_slice(&c);
                    }
                }
            }
        }
    }
    if workload == Bench::Anim {
        let size = (height / 4).max(16);
        let bx = (n as usize * 8) % (width - size);
        let by = height / 2 - size / 2;
        let shade = (n & 0xff) as u8;
        for y in by..by + size {
            for x in bx..bx + size {
                data[(y * width + x) * 4..][..4].copy_from_slice(&[0x40 ^ shade, 0x40, 0xe0 ^ shade, 255]);
            }
        }
    }
    draw_marker(&mut data, width, marker_bits(n, ms));
    Frame { width, height, stride: row_bytes, format: PixelFormat::Bgrx, data }
}

fn draw_marker(data: &mut [u8], width: usize, bits: u64) {
    let cell = (width / MARKER_SCALE).max(4);
    for bit in 0..MARKER_COLS * MARKER_ROWS {
        let on = (bits >> bit) & 1 == 1;
        let (cx, cy) = (cell * (1 + bit % MARKER_COLS), cell * (1 + bit / MARKER_COLS));
        let v = if on { 255 } else { 0 };
        for y in cy..cy + cell {
            for x in cx..cx + cell {
                data[(y * width + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
}

// ---- Audio: a 440 Hz tone ----

pub fn start_tone(packets: AudioPackets) {
    std::thread::spawn(move || {
        let per_packet = Duration::from_micros(
            AUDIO_FRAMES_PER_PACKET as u64 * 1_000_000 / AUDIO_SAMPLE_RATE as u64,
        );
        let mut next = Instant::now();
        let mut t = 0u64;
        for seq in 0u32.. {
            next += per_packet;
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
            let pcm: Vec<i16> = (0..AUDIO_FRAMES_PER_PACKET)
                .flat_map(|_| {
                    t += 1;
                    let v = ((t as f64 * 440.0 * std::f64::consts::TAU / AUDIO_SAMPLE_RATE as f64)
                        .sin()
                        * 8000.0) as i16;
                    [v, v]
                })
                .collect();
            let _ = packets.send(Arc::new(audio_packet(seq, now_us(), 2, &pcm)));
        }
    });
}

// ---- Input: recorded ----

#[derive(Default)]
struct Recorded {
    events: Vec<serde_json::Value>,
    pastes: u32,
}

static INPUT: Mutex<Recorded> = Mutex::new(Recorded {
    events: Vec::new(),
    pastes: 0,
});

#[derive(Default)]
struct InputGate { blocked: bool, entered: u64, completion: Option<crate::text_input::Completion> }
static INPUT_GATE: (Mutex<InputGate>, std::sync::Condvar) = (Mutex::new(InputGate { blocked: false, entered: 0, completion: None }), std::sync::Condvar::new());
#[derive(serde::Deserialize)]
struct GateRequest { blocked: bool, completion: Option<String> }

pub struct FakeInput;

impl InputSink for FakeInput {
    fn execute_step(&mut self, op: &crate::input_queue::Operation) -> crate::text_input::Completion {
        use crate::input_queue::{Operation, Physical, Control};
        if matches!(op, Operation::Scalar(_)) {
            let (lock, wake) = &INPUT_GATE;
            let mut gate = lock.lock().unwrap();
            gate.entered += 1;
            while gate.blocked { gate = wake.wait(gate).unwrap(); }
            if let Some(completion) = gate.completion.take() { return completion; }
        }
        match op {
            Operation::Ownership => {},
            Operation::Scalar(ch) => {
                INPUT.lock().unwrap().events.push(serde_json::json!({"t":"text", "text":ch.to_string()}));
            }
            Operation::Down(p) | Operation::Repeat(p) | Operation::Up(p) => {
                let down = !matches!(op, Operation::Up(_));
                self.handle(match (*p, down) {
                    (Physical::Key(keycode), true) => InputEvent::KeyDown { keycode },
                    (Physical::Key(keycode), false) => InputEvent::KeyUp { keycode },
                    (Physical::Button(button), true) => InputEvent::MouseDown { button },
                    (Physical::Button(button), false) => InputEvent::MouseUp { button },
                });
            }
            Operation::Control(Control::Event(ev)) => self.handle(*ev),
            Operation::Control(Control::Paste) => self.paste(),
            Operation::Control(Control::Move { display, x, y }) => self.move_on_display(*display, *x, *y),
            Operation::Control(_) => {},
        }
        crate::text_input::Completion::Completed
    }

    fn handle(&mut self, ev: InputEvent) {
        let json = match ev {
            InputEvent::MouseMove { x, y, stream } => {
                *POINTER.lock().unwrap() = Some(PointerAt::Stream(stream, x, y));
                serde_json::json!({"t": "move", "x": x, "y": y, "stream": stream})
            }
            InputEvent::MouseDown { button } => serde_json::json!({"t": "down", "button": button}),
            InputEvent::MouseUp { button } => serde_json::json!({"t": "up", "button": button}),
            InputEvent::Scroll { dx, dy } => serde_json::json!({"t": "scroll", "dx": dx, "dy": dy}),
            InputEvent::KeyDown { keycode } => {
                serde_json::json!({"t": "keydown", "keycode": keycode})
            }
            InputEvent::KeyUp { keycode } => serde_json::json!({"t": "keyup", "keycode": keycode}),
            InputEvent::MouseMoveBy { dx, dy } => serde_json::json!({"t": "moveBy", "dx": dx, "dy": dy}),
            InputEvent::Pen { near, pressure, tilt_x, tilt_y, twist } => {
                serde_json::json!({"t": "pen", "near": near, "pressure": pressure, "tiltX": tilt_x, "tiltY": tilt_y, "twist": twist})
            }
            InputEvent::Gamepad { pad, kind, index, value } => {
                serde_json::json!({"t": "gamepad", "pad": pad, "kind": kind, "index": index, "value": value})
            }
            // Put together by the server and typed with type_text.
            InputEvent::Text { .. } => return,
        };
        INPUT.lock().unwrap().events.push(json);
    }

    fn paste(&mut self) {
        INPUT.lock().unwrap().pastes += 1;
    }

    fn release_all(&mut self) {}

    fn set_displays(&mut self, _displays: &[u32]) {}

    fn move_on_display(&mut self, display: u32, x: f32, y: f32) {
        *POINTER.lock().unwrap() = Some(PointerAt::Display(display, x, y));
        INPUT.lock().unwrap().events.push(serde_json::json!({"t": "move", "x": x, "y": y, "display": display}));
    }

    fn type_text(&mut self, text: &str) -> bool {
        INPUT.lock().unwrap().events.push(serde_json::json!({"t": "text", "text": text}));
        true
    }
}

// ---- Pointer: wherever input last put it, as an 8×8 square ----

#[derive(Clone, Copy, PartialEq)]
enum PointerAt {
    Stream(u8, f32, f32),
    Display(u32, f32, f32),
}

static POINTER: Mutex<Option<PointerAt>> = Mutex::new(None);

#[derive(Default)]
pub struct CursorPoller {
    shown: bool,
}

impl crate::cursor::Poll for CursorPoller {
    fn poll(&mut self, shared: &crate::server::Shared) -> Option<crate::cursor::Sample> {
        let at = (*POINTER.lock().unwrap())?;
        let (d, x, y) = match at {
            PointerAt::Stream(s, x, y) => (shared.video.streams().get(s as usize).cloned()?, x, y),
            PointerAt::Display(id, x, y) => (shared.video.displays().ok()?.into_iter().find(|d| d.id == id)?, x, y),
        };
        let image = (!std::mem::replace(&mut self.shown, true)).then(|| crate::cursor::Image {
            width: 8,
            height: 8,
            hot_x: 0,
            hot_y: 0,
            scale: 1.0,
            rgba: [0, 0, 0, 255].repeat(64),
        });
        Some(crate::cursor::Sample {
            at: (d.x + x as f64 * d.points_width, d.y + y as f64 * d.points_height),
            visible: true,
            image,
        })
    }
}

// ---- Clipboard: in memory ----

static CLIPBOARD: Mutex<String> = Mutex::new(String::new());
/// The rest of the clipboard: its rich text and image.
static RICH: Mutex<(Option<String>, Option<Vec<u8>>)> = Mutex::new((None, None));

pub struct FakeClipboard;

impl Clipboard for FakeClipboard {
    fn set_text(&self, text: &str) {
        *CLIPBOARD.lock().unwrap() = text.to_owned();
        *RICH.lock().unwrap() = (None, None);
    }

    fn set(&self, c: &crate::clip::Clip) {
        *CLIPBOARD.lock().unwrap() = c.text.clone();
        *RICH.lock().unwrap() = (c.html.clone(), c.image.clone());
    }

    fn image_png(&self) -> Option<Vec<u8>> {
        RICH.lock().unwrap().1.clone()
    }
}

/// What tests read and write at /test/clipboard/rich (the image as base64).
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct RichClip {
    #[serde(default)]
    text: String,
    #[serde(default)]
    html: Option<String>,
    #[serde(default)]
    image: Option<String>,
}

// ---- Files ----

/// What "Get files from Mac" returns in test mode: a generated file.
pub fn picked_files() -> Vec<PathBuf> {
    let path = std::env::temp_dir().join(format!("glass-test-pick-{}.txt", std::process::id()));
    let _ = std::fs::write(&path, b"hello from the glass test host\n");
    vec![path]
}

// ---- Test-only HTTP routes ----

#[derive(serde::Deserialize)]
struct KindQuery {
    kind: String,
}

#[derive(serde::Deserialize)]
struct DecideQuery {
    id: Option<u64>,
    allow: bool,
}

#[derive(serde::Deserialize)]
struct PauseQuery {
    on: bool,
}

#[derive(serde::Deserialize)]
struct DropQuery {
    frames: u32,
}

pub fn routes(events: broadcast::Sender<ServerMsg>, downloads: PathBuf, input: crate::input_dispatch::Dispatch) -> axum::Router {
    axum::Router::new()
        .route("/test/input-gate", get(|| async {
            let gate = INPUT_GATE.0.lock().unwrap();
            Json(serde_json::json!({"entered":gate.entered,"blocked":gate.blocked}))
        }).post(|Json(request): Json<GateRequest>| async move {
            use crate::text_input::Completion;
            let mut gate = INPUT_GATE.0.lock().unwrap();
            gate.blocked = request.blocked;
            gate.completion = request.completion.as_deref().map(|value| match value {
                "partial" => Completion::Partial, "failed" => Completion::Failed,
                "unknown" => Completion::Unknown, "unsupported" => Completion::Unsupported,
                _ => Completion::Completed,
            });
            INPUT_GATE.1.notify_all();
        }))
        .route(
            "/test/input",
            get(move || { let input = input.clone(); async move {
                let r = INPUT.lock().unwrap();
                Json(serde_json::json!({"events": r.events, "pastes": r.pastes, "pending": input.pending_count(), "owners":input.owner_count()}))
            } })
            .delete(|| async {
                *INPUT.lock().unwrap() = Recorded::default();
            }),
        )
        .route(
            "/test/clipboard",
            get(|| async { CLIPBOARD.lock().unwrap().clone() }).post({
                let events = events.clone();
                move |text: String| {
                    let events = events.clone();
                    async move {
                        FakeClipboard.set_text(&text);
                        let _ = events.send(ServerMsg::Clipboard { text, html: None, image: false });
                    }
                }
            }),
        )
        .route(
            "/test/clipboard/rich",
            get(|| async {
                use base64::Engine;
                let (html, image) = RICH.lock().unwrap().clone();
                Json(RichClip {
                    text: CLIPBOARD.lock().unwrap().clone(),
                    html,
                    image: image.map(|i| base64::engine::general_purpose::STANDARD.encode(i)),
                })
            })
            .post({
                let events = events.clone();
                move |Json(c): Json<RichClip>| {
                    let events = events.clone();
                    async move {
                        use base64::Engine;
                        let image = c.image.and_then(|i| base64::engine::general_purpose::STANDARD.decode(i).ok());
                        let has_image = image.is_some();
                        FakeClipboard.set(&crate::clip::Clip { text: c.text.clone(), html: c.html.clone(), image });
                        let _ = events.send(ServerMsg::Clipboard { text: c.text, html: c.html, image: has_image });
                    }
                }
            }),
        )
        .route(
            "/test/drop",
            post(|Query(q): Query<DropQuery>| async move { DROP_FRAMES.store(q.frames, Ordering::Relaxed) }),
        )
        .route("/test/updates", get(|| async { Json(serde_json::json!({"count": UPDATES.load(Ordering::Relaxed)})) }))
        .route("/test/fit", get(|| async { Json(serde_json::json!({"requests": *FITS.lock().unwrap()})) }))
        .route("/test/bitrate", get(|| async { Json(serde_json::json!({
            "bps": BITRATE.load(Ordering::Relaxed),
            "keyframes": KEYFRAMES.load(Ordering::Relaxed),
            "biggestKeyframe": BIGGEST_KEYFRAME.load(Ordering::Relaxed),
        })) }))
        .route(
            "/test/records",
            get(|Query(q): Query<KindQuery>| async move {
                let r = RECORDS.lock().unwrap();
                let list: Vec<_> = r.iter().filter(|(k, _)| *k == q.kind).map(|(_, v)| v.clone()).collect();
                Json(serde_json::json!({"records": list}))
            }),
        )
        .route(
            "/test/approvals",
            get(|| async { Json(serde_json::json!({"pending": crate::approval::pending()})) }).post(
                |Query(q): Query<DecideQuery>| async move {
                    // No id: the oldest waiting request.
                    let id = q.id.or_else(|| crate::approval::pending().first().map(|r| r.id));
                    let ok = id.is_some_and(|id| crate::approval::decide(id, q.allow));
                    Json(serde_json::json!({"ok": ok}))
                },
            ),
        )
        .route(
            "/test/pause",
            post(|Query(q): Query<PauseQuery>| async move { PAUSED.store(q.on, Ordering::Relaxed) }),
        )
        .route("/test/corrupt", post(|| async { CORRUPT_NEXT.store(true, Ordering::Relaxed) }))
        .route(
            "/test/deaf",
            post(|Query(q): Query<PauseQuery>| async move { DEAF.store(q.on, Ordering::Relaxed) }),
        )
        .route(
            "/test/downloads",
            get(move || {
                let dir = downloads.clone();
                async move {
                    let files: Vec<serde_json::Value> = std::fs::read_dir(&dir)
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter_map(|e| {
                            let meta = e.metadata().ok()?;
                            Some(serde_json::json!({"name": e.file_name().to_string_lossy(), "size": meta.len()}))
                        })
                        .collect();
                    Json(serde_json::json!({"files": files}))
                }
            }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bench_size_must_be_even_and_sane() {
        assert_eq!(parse_size("1280x720"), Some((1280, 720)));
        assert_eq!(parse_size("1281x720"), None);
        assert_eq!(parse_size("64x64"), None);
        assert_eq!(parse_size("wide"), None);
    }

    /// The marker reads back as written: what web/bench.js does, on the raw frame.
    #[test]
    fn marker_round_trips() {
        let (w, h) = (1280, 720);
        for (n, ms) in [(0, 0), (1, 123_456), (0xffff, 0xff_ffff), (70_000, 1 << 30)] {
            let frame = bench_render(Bench::Scroll, w, h, 0);
            let mut data = frame.data;
            draw_marker(&mut data, w, marker_bits(n, ms));
            let cell = w / MARKER_SCALE;
            let mut v = 0u64;
            for bit in 0..MARKER_COLS * MARKER_ROWS {
                let (x, y) = (cell * (1 + bit % MARKER_COLS) + cell / 2, cell * (1 + bit / MARKER_COLS) + cell / 2);
                if data[(y * w + x) * 4] > 127 {
                    v |= 1 << bit;
                }
            }
            assert_eq!(v, marker_bits(n, ms));
            assert_eq!(v & 0xffff, n & 0xffff);
            assert_eq!((v >> 16) & 0xff_ffff, ms & 0xff_ffff);
        }
        // A blank corner isn't a marker.
        assert_ne!(marker_bits(0, 0) >> 40, 0);
    }
}
