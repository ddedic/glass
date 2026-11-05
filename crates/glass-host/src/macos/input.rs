//! Injects client input as system-level CGEvents. Requires Accessibility permission.

use glass_proto::InputEvent;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventMouseSubtype, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton,
    CGScrollEventUnit,
};

const KVK_V: u16 = 0x09;
// kVK_* keycodes for modifiers.
const KVK_COMMAND: u16 = 0x37;
const KVK_SHIFT: u16 = 0x38;
const KVK_OPTION: u16 = 0x3A;
const KVK_CONTROL: u16 = 0x3B;
const KVK_RIGHT_COMMAND: u16 = 0x36;
const KVK_RIGHT_SHIFT: u16 = 0x3C;
const KVK_RIGHT_OPTION: u16 = 0x3D;
const KVK_RIGHT_CONTROL: u16 = 0x3E;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionDeclareUserActivity(name: *const std::ffi::c_void, user_type: u32, id: *mut u32) -> i32;
}

/// kIOPMUserActiveLocal: what a person at the keyboard declares (the remote kind doesn't
/// wake the display).
const USER_ACTIVE_LOCAL: u32 = 0;

/// Tells macOS someone is using the computer. Posted events alone don't count while the
/// display sleeps, and the lock screen won't check a password until someone does: without
/// this, typing the right password from a viewer fails as "incorrect". At most once a second.
fn declare_user_active() {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    static ID: AtomicU32 = AtomicU32::new(0);
    static AT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    if now.saturating_sub(AT.swap(now, Ordering::Relaxed)) < 1000 {
        return;
    }
    let name = objc2_core_foundation::CFString::from_str("Glass remote input");
    let mut id = ID.load(Ordering::Relaxed);
    // SAFETY: a valid CFString; the same assertion is updated each time.
    unsafe { IOPMAssertionDeclareUserActivity((&*name as *const objc2_core_foundation::CFString).cast(), USER_ACTIVE_LOCAL, &mut id) };
    ID.store(id, Ordering::Relaxed);
}

/// Without Accessibility permission, posted events are silently dropped.
pub fn has_accessibility() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// kCGMouseEventClickState
const CLICK_STATE: CGEventField = CGEventField(1);
/// kCGMouseEventDeltaX / DeltaY
const DELTA_X: CGEventField = CGEventField(4);
const DELTA_Y: CGEventField = CGEventField(5);

fn contains(b: CGRect, p: CGPoint) -> bool {
    p.x >= b.origin.x && p.y >= b.origin.y && p.x < b.origin.x + b.size.width && p.y < b.origin.y + b.size.height
}

pub struct Injector {
    source: Option<CFRetained<CGEventSource>>,
    /// The streamed displays, indexed by stream number. Their bounds are looked up as
    /// they're used: a display we just added or mirrored settles a moment later.
    displays: Vec<CGDirectDisplayID>,
    /// A streamed window's global bounds, instead of the displays.
    window: Option<CGRect>,
    pos: CGPoint,
    buttons: [bool; 3],
    flags: CGEventFlags,
    last_click: Option<(u8, std::time::Instant, CGPoint)>,
    click_count: i64,
    /// Relative motion for the next move event (games read these, not the position).
    delta: (i64, i64),
    /// A pen in proximity (Apple Pencil): mouse events carry its tablet data.
    pen: Option<Pen>,
    /// Scrolling not sent yet, under a point (x, y): trackpads send fractions.
    scroll: (f32, f32),
}

/// The pen's latest state, from `InputEvent::Pen`.
#[derive(Clone, Copy)]
struct Pen {
    pressure: f32,
    tilt: (i8, i8),
    twist: f32,
}

/// Ours, so apps see one consistent tablet pointer.
const PEN_DEVICE_ID: i64 = 0x6c61;
/// NX_TABLET_POINTER_PEN.
const POINTER_TYPE_PEN: i64 = 1;
/// NX_TABLET_CAPABILITY_* (IOLLEvent.h): device id, absolute x and y, buttons, tilt x and
/// y, pressure, rotation.
const PEN_CAPABILITIES: i64 = 0x0001 | 0x0002 | 0x0004 | 0x0040 | 0x0080 | 0x0100 | 0x0400 | 0x2000;

// CGEventSource and CGEventPost are safe to use from any thread; access is serialized by a Mutex.
unsafe impl Send for Injector {}

impl Injector {
    pub fn new(display: CGDirectDisplayID) -> Self {
        Self {
            source: CGEventSource::new(CGEventSourceStateID::HIDSystemState),
            displays: vec![display],
            window: None,
            pos: CGDisplayBounds(display).origin,
            buttons: [false; 3],
            flags: CGEventFlags(0),
            last_click: None,
            click_count: 1,
            delta: (0, 0),
            pen: None,
            scroll: (0.0, 0.0),
        }
    }

    /// Retargets absolute mouse coordinates after the streamed displays change.
    pub fn set_displays(&mut self, displays: &[CGDirectDisplayID]) {
        self.release_all();
        self.displays = displays.to_vec();
        self.window = None;
    }

    /// Global bounds of a stream (or the first one).
    fn bounds(&self, stream: usize) -> Option<CGRect> {
        if let Some(w) = self.window {
            return Some(w);
        }
        self.displays.get(stream).or(self.displays.first()).map(|&d| CGDisplayBounds(d))
    }

    pub fn handle(&mut self, ev: InputEvent) {
        match ev {
            InputEvent::MouseMove { x, y, stream } => {
                let Some(b) = self.bounds(stream as usize) else {
                    return;
                };
                self.move_to(to_global(b, x, y));
            }
            InputEvent::MouseDown { button } | InputEvent::MouseUp { button } => {
                let down = matches!(ev, InputEvent::MouseDown { .. });
                let Some((ty, cg_button)) = mouse_button(button, down) else {
                    return;
                };
                self.buttons[button as usize] = down;
                if down {
                    self.click_count = self.next_click_count(button);
                }
                self.mouse(ty, cg_button);
            }
            InputEvent::Pen { near, pressure, tilt_x, tilt_y, twist } => {
                let was_near = self.pen.is_some();
                if near {
                    self.pen = Some(Pen { pressure, tilt: (tilt_x, tilt_y), twist });
                    if !was_near {
                        self.proximity(true);
                    }
                } else if was_near {
                    self.pen = None;
                    self.proximity(false);
                }
            }
            InputEvent::MouseMoveBy { dx, dy } => {
                // Stay on the display the pointer is on (or the first streamed one).
                let b = match self.window {
                    Some(w) => Some(w),
                    None => self
                        .displays
                        .iter()
                        .map(|&d| CGDisplayBounds(d))
                        .find(|b| contains(*b, self.pos))
                        .or_else(|| self.bounds(0)),
                };
                let Some(b) = b else { return };
                let to = CGPoint {
                    x: (self.pos.x + dx as f64).clamp(b.origin.x, b.origin.x + b.size.width - 1.0),
                    y: (self.pos.y + dy as f64).clamp(b.origin.y, b.origin.y + b.size.height - 1.0),
                };
                self.delta = (dx.round() as i64, dy.round() as i64);
                self.move_to(to);
                self.delta = (0, 0);
            }
            InputEvent::Scroll { dx, dy } => {
                // DOM: +dy scrolls down; CG: +wheel scrolls up. Pixel units are points.
                let Some((x, y)) = whole_points(&mut self.scroll, -dx, -dy) else { return };
                let ev = CGEvent::new_scroll_wheel_event2(self.source.as_deref(), CGScrollEventUnit::Pixel, 2, y, x, 0);
                // Continuous, as a trackpad's: apps scroll by exactly these points rather
                // than treating each event as a wheel notch.
                // Adapted from voxl (MIT, github.com/tembo/voxl): src/mac/input.rs.
                CGEvent::set_integer_value_field(ev.as_deref(), CGEventField::ScrollWheelEventIsContinuous, 1);
                self.post(ev);
            }
            // Routed to crate::gamepad by the server, never here; text goes to type_text.
            InputEvent::Gamepad { .. } | InputEvent::Text { .. } => {}
            InputEvent::KeyDown { keycode } | InputEvent::KeyUp { keycode } => {
                let down = matches!(ev, InputEvent::KeyDown { .. });
                if let Some(flag) = modifier_flag(keycode) {
                    self.flags = if down {
                        CGEventFlags(self.flags.0 | flag.0)
                    } else {
                        CGEventFlags(self.flags.0 & !flag.0)
                    };
                }
                let ev = CGEvent::new_keyboard_event(self.source.as_deref(), keycode, down);
                self.post(ev);
            }
        }
    }

    fn move_to(&mut self, pos: CGPoint) {
        self.pos = pos;
        let (ty, button) = match self.buttons {
            [true, _, _] => (CGEventType::LeftMouseDragged, CGMouseButton::Left),
            [_, _, true] => (CGEventType::RightMouseDragged, CGMouseButton::Right),
            [_, true, _] => (CGEventType::OtherMouseDragged, CGMouseButton::Center),
            _ => (CGEventType::MouseMoved, CGMouseButton::Left),
        };
        self.mouse(ty, button);
    }

    /// Types `text` whatever the keyboard layout: key events carrying the characters
    /// themselves, a few at a time (apps drop longer strings per event).
    pub fn type_text(&mut self, text: &str) {
        let flags = self.flags;
        self.flags = CGEventFlags(0);
        for chunk in unicode_chunks(text) {
            for down in [true, false] {
                let ev = CGEvent::new_keyboard_event(self.source.as_deref(), 0, down);
                // SAFETY: `chunk` outlives the call, and its length is passed with it.
                unsafe { CGEvent::keyboard_set_unicode_string(ev.as_deref(), chunk.len() as _, chunk.as_ptr()) };
                self.post(ev);
            }
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        self.flags = flags;
    }

    /// Pastes the clipboard: Cmd+V, using the Cmd the client is already holding.
    pub fn paste(&mut self) {
        self.tap_key(KVK_V);
    }

    /// Presses and releases a key with the currently held modifiers.
    pub fn tap_key(&mut self, keycode: u16) {
        self.handle(InputEvent::KeyDown { keycode });
        self.handle(InputEvent::KeyUp { keycode });
    }

    /// Releases anything still held, e.g. when a client disconnects mid-drag.
    pub fn release_all(&mut self) {
        for button in 0..3u8 {
            if self.buttons[button as usize] {
                self.handle(InputEvent::MouseUp { button });
            }
        }
        for keycode in [
            KVK_COMMAND,
            KVK_SHIFT,
            KVK_OPTION,
            KVK_CONTROL,
            KVK_RIGHT_COMMAND,
            KVK_RIGHT_SHIFT,
            KVK_RIGHT_OPTION,
            KVK_RIGHT_CONTROL,
        ] {
            if modifier_flag(keycode).is_some_and(|f| self.flags.0 & f.0 != 0) {
                self.handle(InputEvent::KeyUp { keycode });
            }
        }
    }

    fn next_click_count(&mut self, button: u8) -> i64 {
        let now = std::time::Instant::now();
        let count = match self.last_click {
            Some((b, t, p))
                if b == button
                    && now.duration_since(t).as_millis() < 400
                    && (p.x - self.pos.x).abs() < 5.0
                    && (p.y - self.pos.y).abs() < 5.0 =>
            {
                self.click_count + 1
            }
            _ => 1,
        };
        self.last_click = Some((button, now, self.pos));
        count
    }

    fn mouse(&self, ty: CGEventType, button: CGMouseButton) {
        let ev = CGEvent::new_mouse_event(self.source.as_deref(), ty, self.pos, button);
        CGEvent::set_integer_value_field(ev.as_deref(), CLICK_STATE, self.click_count);
        if self.delta != (0, 0) {
            CGEvent::set_integer_value_field(ev.as_deref(), DELTA_X, self.delta.0);
            CGEvent::set_integer_value_field(ev.as_deref(), DELTA_Y, self.delta.1);
        }
        if let Some(pen) = self.pen {
            // A tablet point: NSEvent.pressure, .tilt and .rotation in drawing apps.
            let pressure = if self.buttons[0] { pen.pressure.max(0.01) as f64 } else { 0.0 };
            let ev = ev.as_deref();
            CGEvent::set_integer_value_field(ev, CGEventField::MouseEventSubtype, CGEventMouseSubtype::TabletPoint.0 as i64);
            CGEvent::set_double_value_field(ev, CGEventField::MouseEventPressure, pressure);
            CGEvent::set_double_value_field(ev, CGEventField::TabletEventPointPressure, pressure);
            CGEvent::set_double_value_field(ev, CGEventField::TabletEventTiltX, pen.tilt.0 as f64 / 90.0);
            CGEvent::set_double_value_field(ev, CGEventField::TabletEventTiltY, pen.tilt.1 as f64 / 90.0);
            CGEvent::set_double_value_field(ev, CGEventField::TabletEventRotation, pen.twist as f64);
            CGEvent::set_integer_value_field(ev, CGEventField::TabletEventPointButtons, self.buttons[0] as i64);
            CGEvent::set_integer_value_field(ev, CGEventField::TabletEventDeviceID, PEN_DEVICE_ID);
        }
        self.post(ev);
    }

    /// The pen came near the screen (or left): apps learn a tablet pen is in use.
    fn proximity(&self, enter: bool) {
        let ev = CGEvent::new(self.source.as_deref());
        let e = ev.as_deref();
        CGEvent::set_type(e, CGEventType::TabletProximity);
        CGEvent::set_location(e, self.pos);
        CGEvent::set_integer_value_field(e, CGEventField::MouseEventSubtype, CGEventMouseSubtype::TabletProximity.0 as i64);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventDeviceID, PEN_DEVICE_ID);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventSystemTabletID, PEN_DEVICE_ID);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventPointerID, 1);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventPointerType, POINTER_TYPE_PEN);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventCapabilityMask, PEN_CAPABILITIES);
        CGEvent::set_integer_value_field(e, CGEventField::TabletProximityEventEnterProximity, enter as i64);
        self.post(ev);
    }

    fn post(&self, ev: Option<CFRetained<CGEvent>>) {
        if let Some(ev) = ev {
            declare_user_active();
            CGEvent::set_flags(Some(&ev), self.flags);
            // Lets privacy mode's event tap tell these from the local keyboard and mouse.
            CGEvent::set_integer_value_field(
                Some(&ev),
                CGEventField(super::privacy::USER_DATA_FIELD),
                super::privacy::INJECTED_MARK,
            );
            CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
            super::front::injected();
        }
    }
}

/// CGEvent strings are short, but a UTF-16 surrogate pair must stay in one event.
fn unicode_chunks(text: &str) -> Vec<Vec<u16>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    for ch in text.chars() {
        if chunk.len() + ch.len_utf16() > 16 {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

/// A point given as 0..1 of a stream, in global desktop points within `b` (a display,
/// or a streamed window).
fn to_global(b: CGRect, x: f32, y: f32) -> CGPoint {
    CGPoint {
        x: b.origin.x + x.clamp(0.0, 1.0) as f64 * b.size.width,
        y: b.origin.y + y.clamp(0.0, 1.0) as f64 * b.size.height,
    }
}

/// Adds a scroll to what's waiting in `acc` and takes the whole points out of it, keeping
/// the fraction for next time; `None` while there's less than a point either way.
fn whole_points(acc: &mut (f32, f32), dx: f32, dy: f32) -> Option<(i32, i32)> {
    if !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    acc.0 += dx;
    acc.1 += dy;
    let (x, y) = (acc.0.trunc(), acc.1.trunc());
    acc.0 -= x;
    acc.1 -= y;
    (x != 0.0 || y != 0.0).then_some((x as i32, y as i32))
}

/// DOM button numbering: 0 left, 1 middle, 2 right.
fn mouse_button(button: u8, down: bool) -> Option<(CGEventType, CGMouseButton)> {
    Some(match (button, down) {
        (0, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
        (0, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
        (2, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
        (2, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
        (1, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
        (1, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        _ => return None,
    })
}

fn modifier_flag(keycode: u16) -> Option<CGEventFlags> {
    Some(match keycode {
        KVK_COMMAND | KVK_RIGHT_COMMAND => CGEventFlags::MaskCommand,
        KVK_SHIFT | KVK_RIGHT_SHIFT => CGEventFlags::MaskShift,
        KVK_OPTION | KVK_RIGHT_OPTION => CGEventFlags::MaskAlternate,
        KVK_CONTROL | KVK_RIGHT_CONTROL => CGEventFlags::MaskControl,
        _ => return None,
    })
}

impl crate::media::InputSink for Injector {
    fn handle(&mut self, ev: glass_proto::InputEvent) {
        Injector::handle(self, ev)
    }
    fn paste(&mut self) {
        Injector::paste(self)
    }
    fn release_all(&mut self) {
        Injector::release_all(self)
    }
    fn set_window_rect(&mut self, rect: Option<[f64; 4]>) {
        // `None`: set_displays already put the display bounds back.
        // No release here: the window may be moving because it's being dragged.
        if let Some([x, y, w, h]) = rect {
            self.window = Some(CGRect {
                origin: CGPoint { x, y },
                size: objc2_core_foundation::CGSize {
                    width: w,
                    height: h,
                },
            });
        }
    }
    fn set_displays(&mut self, displays: &[u32]) {
        Injector::set_displays(self, displays)
    }
    fn move_on_display(&mut self, display: u32, x: f32, y: f32) {
        self.move_to(to_global(CGDisplayBounds(display), x, y));
    }
    fn type_text(&mut self, text: &str) -> bool {
        Injector::type_text(self, text);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_chunks_never_split_a_surrogate_pair() {
        let chunks = unicode_chunks("123456789012345😀x");
        assert_eq!(chunks.len(), 2);
        assert_eq!(String::from_utf16(&chunks[0]).unwrap(), "123456789012345");
        assert_eq!(String::from_utf16(&chunks[1]).unwrap(), "😀x");
    }

    #[test]
    fn window_coordinates_land_inside_the_window() {
        let window = CGRect {
            origin: CGPoint { x: 100.0, y: 50.0 },
            size: objc2_core_foundation::CGSize {
                width: 800.0,
                height: 600.0,
            },
        };
        let p = to_global(window, 0.5, 0.25);
        assert_eq!((p.x, p.y), (500.0, 200.0));
        let edge = to_global(window, 1.5, -1.0);
        assert_eq!((edge.x, edge.y), (900.0, 50.0), "clamped to the window");
    }

    #[test]
    fn scroll_fractions_add_up_instead_of_vanishing() {
        let mut acc = (0.0, 0.0);
        // A slow trackpad: a third of a point a frame used to round to nothing, every time.
        assert_eq!(whole_points(&mut acc, 0.0, 0.4), None);
        assert_eq!(whole_points(&mut acc, 0.0, 0.4), None);
        assert_eq!(whole_points(&mut acc, 0.0, 0.4), Some((0, 1)));
        assert_eq!(whole_points(&mut acc, -2.5, 0.0), Some((-2, 0)));
        assert_eq!(whole_points(&mut acc, -0.5, 0.0), Some((-1, 0)));
        assert_eq!(whole_points(&mut acc, f32::NAN, 1.0), None);
        assert!(acc.0.abs() < 1e-6 && (acc.1 - 0.2).abs() < 1e-5);
    }
}

