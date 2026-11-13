//! Types shared by the platform capture/encode code and the server.

use std::sync::Arc;

use tokio::sync::broadcast;

/// A capturable display. `id` is platform-specific (CGDirectDisplayID on macOS, the
/// portal stream index on Linux).
#[derive(Debug, Clone, serde::Serialize)]
pub struct DisplayInfo {
    pub id: u32,
    pub width: usize,
    pub height: usize,
    /// Position and size in the desktop's display arrangement (global points).
    pub x: f64,
    pub y: f64,
    pub points_width: f64,
    pub points_height: f64,
    pub main: bool,
    pub builtin: bool,
}

pub struct EncodedFrame {
    /// Display stream number, set by the stream that owns the encoder.
    pub stream: u8,
    /// Annex B H.264 with SPS/PPS before every keyframe.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub capture_us: u64,
    /// HEVC (Annex B, VPS/SPS/PPS before keyframes) instead of H.264.
    pub hevc: bool,
    /// Re-encodes a picture that hasn't changed (see `glass_proto::FLAG_REFRESH`).
    pub refresh: bool,
}

/// A window that can be streamed on its own (single-app streaming).
#[derive(Debug, Clone, serde::Serialize)]
pub struct WindowInfo {
    pub id: u32,
    pub app: String,
    pub title: String,
    /// Pixel size.
    pub width: usize,
    pub height: usize,
    /// Position and size on the desktop (global points), for input mapping.
    pub x: f64,
    pub y: f64,
    pub points_width: f64,
    pub points_height: f64,
}

/// Fixed-size PCM packets (see `glass_proto::audio_packet`), fanned out to sessions.
pub type AudioPackets = broadcast::Sender<Arc<Vec<u8>>>;

/// What to stream: one display (the main one when `None`) or every display.
#[derive(Debug, Clone, Copy)]
pub enum Layout {
    Single(Option<u32>),
    All,
    /// One window, by `WindowInfo::id`.
    Window(u32),
}

/// A source of encoded display streams (the platform capturer, or the test pattern).
pub trait VideoSource: Send + Sync {
    /// The displays being streamed; index = stream number.
    fn streams(&self) -> Vec<DisplayInfo>;
    /// Requests a keyframe on one stream, or all of them.
    fn request_keyframe(&self, stream: Option<u8>);
    /// Restarts capture with `layout`. May block.
    fn set_layout(&self, layout: Layout) -> anyhow::Result<Vec<DisplayInfo>>;
    /// Every capturable display. May block.
    fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>>;
    /// Target bitrate for each stream, in bits per second (adaptive rate control).
    fn set_bitrate(&self, bps: u32);
    /// Re-shares the bitrate between displays (called every second).
    fn rebalance(&self) {}
    /// Restarts any capture that ended, e.g. when its display slept (called every second).
    /// May block.
    fn heal(&self) {}
    /// Encodes each stream at this width (`None` = native), from what viewers show.
    fn set_target_widths(&self, _widths: &[Option<u32>]) {}
    /// Frames per second viewers want (crate::refresh); capped by each display's refresh.
    fn set_max_fps(&self, _fps: u32) {}
    /// The colour profile of a stream's pictures when it isn't sRGB (Display P3), for
    /// tagging lossless pictures.
    fn icc(&self, _stream: u8) -> Option<std::sync::Arc<Vec<u8>>> {
        None
    }
    /// Can this source stream single windows?
    fn supports_windows(&self) -> bool {
        false
    }
    /// Windows that can be streamed on their own. May block.
    fn windows(&self) -> anyhow::Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
    /// Encode in HEVC (`true`) or H.264; returns the codec now in use.
    fn set_hevc(&self, _hevc: bool) -> bool {
        false
    }
    /// The latest captured picture of stream `stream`, in full colour, for lossless refresh
    /// (`None` when it isn't captured in full colour). May block briefly.
    fn raw_frame(&self, _stream: u8) -> Option<std::sync::Arc<crate::lossless::RawFrame>> {
        None
    }
    /// Bumped each time stream `stream` captures a new picture.
    fn frame_generation(&self, _stream: u8) -> Option<u64> {
        None
    }
    /// A picture of display `id` (by `DisplayInfo::id`), shown or not, at most `max` pixels
    /// (aspect kept): display previews and agents' screenshots. May block. `None` when
    /// there's none (yet).
    fn picture(&self, _id: u32, _max: [u32; 2]) -> Option<crate::lossless::RawFrame> {
        None
    }
    /// Makes sure display `id`'s changes reach `crate::damage` (an agent is waiting on it),
    /// starting a capture of it if none runs; `false` when this source can't, and pictures
    /// of it are compared instead. Called on each look; may block.
    fn track_damage(&self, _id: u32) -> bool {
        false
    }
    /// Capture in full colour (BGRA), so `raw_frame` has pictures to send losslessly.
    fn set_full_colour(&self, _on: bool) {}
    /// Draw the pointer into the picture (`crate::cursor` decides), or leave it out.
    fn set_cursor_baked(&self, _on: bool) {}
    /// Can this source provide raw frames at all?
    fn supports_lossless(&self) -> bool {
        false
    }
    /// Sharp text: HEVC 4:2:2 instead of 4:2:0 while HEVC is on; returns whether it's in use.
    fn set_rext(&self, _on: bool) -> bool {
        false
    }
    /// Can this source encode HEVC 4:2:2 (Range Extensions)?
    fn supports_rext(&self) -> bool {
        false
    }
    /// Can this source encode HEVC at all?
    fn supports_hevc(&self) -> bool {
        false
    }
}

/// Where client input goes (the platform injector, or a recorder in tests).
pub trait InputSink: Send {
    /// Bounded native executor API. Only input_dispatch's worker may call this API.
    /// Legacy backend helpers must never be used by producers.
    /// Unsupported guarantees no attempt; all other failures prohibit fallback/replay.
    /// Completed means native submission, not delivery to an application. Controls that
    /// reset routes/holds are coordinated by the worker before execution.
    /// The worker must invalidate a route on Unknown (and any unsuccessful native Up).
    fn execute_step(&mut self, operation: &crate::input_queue::Operation) -> crate::text_input::Completion {
        if matches!(operation, crate::input_queue::Operation::Ownership) {
            crate::text_input::Completion::Completed
        } else {
            crate::text_input::Completion::Unsupported
        }
    }

    /// One release-only recovery on the executor that attempted this step. Never reroute
    /// or replay an uncertain Down on a replacement executor.
    fn recover_step(&mut self, operation: &crate::input_queue::Operation) {
        if let crate::input_queue::Operation::Down(physical) = operation {
            let _ = self.execute_step(&crate::input_queue::Operation::Up(*physical));
        }
        self.release_all();
    }
    fn reset_state(&mut self) -> crate::text_input::Completion {
        self.release_all();
        crate::text_input::Completion::Completed
    }
    fn handle(&mut self, ev: glass_proto::InputEvent);
    /// Legacy compatibility only; producers submit through input_dispatch.
    fn handle_from(&mut self, _owner: u64, ev: glass_proto::InputEvent) { self.handle(ev); }
    fn release_owner(&mut self, _owner: u64) {}
    /// Pastes the host clipboard (the platform's paste shortcut).
    fn paste(&mut self);
    /// Releases anything still held, e.g. when a client disconnects mid-drag.
    fn release_all(&mut self);
    /// Retargets absolute mouse coordinates to the streamed displays.
    fn set_displays(&mut self, displays: &[u32]);
    /// Maps absolute coordinates on stream 0 to this rectangle of the desktop (global
    /// points) instead of a display: a streamed window. `None` goes back to displays.
    fn set_window_rect(&mut self, _rect: Option<[f64; 4]>) {}
    /// Moves the pointer to `x`, `y` (0..1) of display `display`, streamed or not (agents).
    fn move_on_display(&mut self, display: u32, x: f32, y: f32);
    /// Types `text` as is, any characters; `false` when this sink can't (the caller then
    /// presses keys for it). Linux can't: uinput sends key codes through the desktop's own
    /// keyboard layout, with no way to type a character the layout lacks (no XTest on
    /// Wayland, and Glass doesn't drive the RemoteDesktop portal, which takes keysyms), so
    /// only characters on a US keyboard are typed there.
    fn type_text(&mut self, _text: &str) -> bool {
        false
    }
}

/// The host clipboard: text, and where the platform allows, rich text and images.
pub trait Clipboard: Send + Sync {
    fn set_text(&self, text: &str);
    /// Text, HTML and an image together (what a viewer copied).
    fn set(&self, clip: &crate::clip::Clip) {
        self.set_text(&clip.text);
    }
    /// The clipboard's image as PNG, if it holds one.
    fn image_png(&self) -> Option<Vec<u8>> {
        None
    }
}
