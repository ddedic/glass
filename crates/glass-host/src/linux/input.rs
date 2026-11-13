//! Injects client input through uinput: a virtual keyboard and an absolute pointer.
//! Needs write access to /dev/uinput (`glass setup` installs a udev rule granting it to
//! the logged-in user).

use std::collections::HashSet;
use std::fs::OpenOptions;

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode,
    UinputAbsSetup,
};
use glass_proto::InputEvent as Ev;

use super::keymap::{
    self, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, KEY_LEFTCTRL, KEY_LEFTMETA, KEY_RIGHTMETA, KEY_V,
};
use super::video::{self, Rect};

/// Absolute pointer resolution across the whole desktop layout.
const ABS_MAX: i32 = 65535;
/// Viewers scroll in desktop (logical) pixels; one wheel notch is 120 hi-res units,
/// roughly 40 px.
const HI_RES_PER_PIXEL: f32 = 3.0;

pub fn has_accessibility() -> bool {
    OpenOptions::new().write(true).open("/dev/uinput").is_ok()
}

pub struct Injector {
    step_result: crate::text_input::Completion,
    keyboard: Option<VirtualDevice>,
    pointer: Option<VirtualDevice>,
    /// Logical rect of each streamed display, by stream number.
    bounds: Vec<Rect>,
    /// Bounding box of every monitor; the absolute pointer spans this.
    layout: Rect,
    held: HashSet<u16>,
    buttons: [bool; 3],
    wheel: (f32, f32),
    /// Hi-res wheel movement under a unit, not sent yet (trackpads send fractions).
    hi_res: (f32, f32),
    /// Where the pointer is, in logical desktop coordinates (for relative moves).
    pos: (f64, f64),
}

impl Injector {
    pub fn new(display: u32) -> Self {
        let mut this = Self {
            step_result: crate::text_input::Completion::Completed,
            keyboard: None,
            pointer: None,
            bounds: Vec::new(),
            layout: Rect::default(),
            held: HashSet::new(),
            buttons: [false; 3],
            wheel: (0.0, 0.0),
            hi_res: (0.0, 0.0),
            pos: (0.0, 0.0),
        };
        this.set_displays(&[display]);
        this.ensure_devices();
        this
    }

    /// Creates the virtual devices if they don't exist yet (e.g. access was granted
    /// after the host started).
    fn ensure_devices(&mut self) {
        if self.keyboard.is_some() && self.pointer.is_some() {
            return;
        }
        match build_devices() {
            Ok((keyboard, pointer)) => {
                self.keyboard = Some(keyboard);
                self.pointer = Some(pointer);
                tracing::info!("input: virtual keyboard and pointer ready");
            }
            Err(e) => tracing::warn!("input: can't create uinput devices ({e}); run `glass setup`"),
        }
    }

    pub fn set_displays(&mut self, displays: &[u32]) {
        self.release_all();
        self.bounds = displays
            .iter()
            .filter_map(|&id| video::rect_of(id))
            .collect();
        self.layout = video::layout_rect();
    }

    /// Moves the absolute pointer to `x`, `y` (0..1) of the rectangle `r` (logical coords).
    fn move_in(&mut self, r: video::Rect, x: f32, y: f32) {
        let gx = r.x + x.clamp(0.0, 1.0) as f64 * r.w;
        let gy = r.y + y.clamp(0.0, 1.0) as f64 * r.h;
        self.move_to(gx, gy);
    }

    fn move_to(&mut self, gx: f64, gy: f64) {
        self.pos = (gx, gy);
        let l = self.layout;
        let ax = (((gx - l.x) / l.w.max(1.0)) * ABS_MAX as f64).round() as i32;
        let ay = (((gy - l.y) / l.h.max(1.0)) * ABS_MAX as f64).round() as i32;
        self.pointer(&[
            InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_X.0,
                ax.clamp(0, ABS_MAX),
            ),
            InputEvent::new(
                EventType::ABSOLUTE.0,
                AbsoluteAxisCode::ABS_Y.0,
                ay.clamp(0, ABS_MAX),
            ),
        ]);
    }

    pub fn handle(&mut self, ev: Ev) {
        self.ensure_devices();
        match ev {
            Ev::MouseMove { x, y, stream } => {
                let Some(r) = self
                    .bounds
                    .get(stream as usize)
                    .or(self.bounds.first())
                    .copied()
                else {
                    return;
                };
                self.move_in(r, x, y);
            }
            Ev::MouseMoveBy { dx, dy } => {
                let l = self.layout;
                let x = (self.pos.0 + dx as f64).clamp(l.x, l.x + l.w - 1.0);
                let y = (self.pos.1 + dy as f64).clamp(l.y, l.y + l.h - 1.0);
                self.move_to(x, y);
            }
            // Routed to crate::gamepad by the server, never here; text goes to type_text.
            Ev::Gamepad { .. } | Ev::Text { .. } => {}
            // Pressure needs a tablet device; the pen still works as a mouse.
            Ev::Pen { .. } => {}
            Ev::MouseDown { button } | Ev::MouseUp { button } => {
                let down = matches!(ev, Ev::MouseDown { .. });
                let code = match button {
                    0 => BTN_LEFT,
                    1 => BTN_MIDDLE,
                    2 => BTN_RIGHT,
                    _ => return,
                };
                self.buttons[button as usize] = down;
                self.pointer(&[InputEvent::new(EventType::KEY.0, code, down as i32)]);
            }
            Ev::Scroll { dx, dy } => {
                // DOM: +dy scrolls down. evdev: +wheel scrolls up, +hwheel scrolls right.
                if !dx.is_finite() || !dy.is_finite() {
                    return;
                }
                self.hi_res.0 -= dy * HI_RES_PER_PIXEL;
                self.hi_res.1 += dx * HI_RES_PER_PIXEL;
                let (v, h) = (self.hi_res.0.trunc(), self.hi_res.1.trunc());
                self.hi_res.0 -= v;
                self.hi_res.1 -= h;
                self.wheel.0 += v;
                self.wheel.1 += h;
                let mut events = vec![
                    InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_WHEEL_HI_RES.0,
                        v as i32,
                    ),
                    InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_HWHEEL_HI_RES.0,
                        h as i32,
                    ),
                ];
                // Legacy whole-notch events for apps that ignore hi-res scrolling.
                let notches = (
                    (self.wheel.0 / 120.0).trunc(),
                    (self.wheel.1 / 120.0).trunc(),
                );
                if notches.0 != 0.0 {
                    events.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_WHEEL.0,
                        notches.0 as i32,
                    ));
                    self.wheel.0 -= notches.0 * 120.0;
                }
                if notches.1 != 0.0 {
                    events.push(InputEvent::new(
                        EventType::RELATIVE.0,
                        RelativeAxisCode::REL_HWHEEL.0,
                        notches.1 as i32,
                    ));
                    self.wheel.1 -= notches.1 * 120.0;
                }
                self.pointer(&events);
            }
            Ev::KeyDown { keycode } | Ev::KeyUp { keycode } => {
                let down = matches!(ev, Ev::KeyDown { .. });
                let Some(code) = keymap::to_evdev(keycode) else {
                    return;
                };
                if down {
                    self.held.insert(code);
                } else {
                    self.held.remove(&code);
                }
                self.key(code, down);
            }
        }
    }

    /// Pastes with Ctrl+V. The client triggers this from ⌘V, which arrives here as Super,
    /// so Super is lifted for the paste and restored after.
    pub fn paste(&mut self) {
        let supers: Vec<u16> = [KEY_LEFTMETA, KEY_RIGHTMETA]
            .into_iter()
            .filter(|k| self.held.contains(k))
            .collect();
        for &k in &supers {
            self.key(k, false);
        }
        self.key(KEY_LEFTCTRL, true);
        self.key(KEY_V, true);
        self.key(KEY_V, false);
        self.key(KEY_LEFTCTRL, false);
        for &k in &supers {
            self.key(k, true);
        }
    }

    /// Releases anything still held, e.g. when a client disconnects mid-drag.
    pub fn release_all(&mut self) {
        for button in 0..3u8 {
            if self.buttons[button as usize] {
                self.handle(Ev::MouseUp { button });
            }
        }
        for code in std::mem::take(&mut self.held) {
            self.key(code, false);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        if let Some(kb) = self.keyboard.as_mut()
            && let Err(e) = kb.emit(&[InputEvent::new(EventType::KEY.0, code, down as i32)])
        {
            self.step_result = crate::text_input::Completion::Unknown;
            tracing::warn!("input: keyboard emit failed: {e}");
        }
    }

    fn pointer(&mut self, events: &[InputEvent]) {
        if let Some(p) = self.pointer.as_mut()
            && let Err(e) = p.emit(events)
        {
            self.step_result = crate::text_input::Completion::Unknown;
            tracing::warn!("input: pointer emit failed: {e}");
        }
    }
}

fn build_devices() -> std::io::Result<(VirtualDevice, VirtualDevice)> {
    let keys: AttributeSet<KeyCode> = keymap::all_evdev().map(KeyCode::new).collect();
    let keyboard = VirtualDevice::builder()?
        .name("Glass keyboard")
        .with_keys(&keys)?
        .build()?;

    let buttons: AttributeSet<KeyCode> = [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE]
        .into_iter()
        .map(KeyCode::new)
        .collect();
    let axis = |code| UinputAbsSetup::new(code, AbsInfo::new(0, 0, ABS_MAX, 0, 0, 0));
    let wheels = AttributeSet::from_iter([
        RelativeAxisCode::REL_WHEEL,
        RelativeAxisCode::REL_HWHEEL,
        RelativeAxisCode::REL_WHEEL_HI_RES,
        RelativeAxisCode::REL_HWHEEL_HI_RES,
    ]);
    // ABS_X/ABS_Y plus buttons and no BTN_TOUCH: libinput treats it as an absolute
    // pointer (like a VM's tablet device) spanning the whole layout.
    let pointer = VirtualDevice::builder()?
        .name("Glass pointer")
        .with_keys(&buttons)?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_X))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Y))?
        .with_relative_axes(&wheels)?
        .build()?;
    Ok((keyboard, pointer))
}

impl crate::media::InputSink for Injector {
    fn execute_step(&mut self, operation: &crate::input_queue::Operation) -> crate::text_input::Completion {
        use crate::input_queue::{Control, Operation, Physical};
        use crate::text_input::{Completion, Packet, fallback_packets, transaction};
        // Permissions may have been granted since startup. Recreate only an empty
        // executor: never replace a device behind scheduler-owned physical holds.
        if self.keyboard.is_none() && self.pointer.is_none() && self.held.is_empty() && !self.buttons.iter().any(|b| *b) { self.ensure_devices(); }
        match operation {
            Operation::Ownership => Completion::Completed,
            Operation::Control(control) => {
                match control {
                    Control::MoveAbsolute { .. } => return Completion::Unsupported,
                    Control::Reset => return self.reset_state(),
                    Control::Configure { displays, window } => {
                        let result = self.execute_step(&Operation::Control(Control::Displays(displays.clone())));
                        if result != Completion::Completed { return result; }
                        return self.execute_step(&Operation::Control(Control::Window(*window)));
                    }
                    Control::Display(id) => { self.bounds = video::rect_of(*id).into_iter().collect(); self.layout = video::layout_rect(); }
                    Control::Displays(ids) => { self.bounds = ids.iter().filter_map(|id| video::rect_of(*id)).collect(); self.layout = video::layout_rect(); }
                    Control::Window(rect) => { if let Some([x, y, w, h]) = rect { self.bounds = vec![Rect { x: *x, y: *y, w: *w, h: *h }]; } }
                    Control::Paste => {
                        let ctrl = self.held.contains(&KEY_LEFTCTRL);
                        let v = self.held.contains(&KEY_V);
                        let modifiers: Vec<u16> = [KEY_LEFTMETA, KEY_RIGHTMETA].into_iter().filter(|k| self.held.contains(k)).collect();
                        let mut payload = vec![];
                        if !ctrl { payload.push(Packet::Key(KEY_LEFTCTRL, true)); }
                        payload.extend(crate::text_input::transient(KEY_V, v));
                        if !ctrl { payload.push(Packet::Key(KEY_LEFTCTRL, false)); }
                        let protected = if v { vec![KEY_V] } else { vec![] };
                        let Some(device) = self.keyboard.as_mut() else { return Completion::Failed; };
                        return transaction(&modifiers, false, &payload, &protected, |packets| {
                            let events: Vec<_> = packets.iter().map(|p| match *p { Packet::Key(code, down) => InputEvent::new(EventType::KEY.0, code, down as i32), _ => unreachable!() }).collect();
                            device.emit(&events).ok().map(|_| packets.len())
                        });
                    }
                    Control::Move { display, x, y } => {
                        if self.pointer.is_none() { return Completion::Failed; }
                        let Some(rect) = video::rect_of(*display) else { return Completion::Failed; };
                        self.step_result = Completion::Completed;
                        self.move_in(rect, *x, *y);
                        return self.step_result;
                    }
                    Control::Event(ev) => {
                        if matches!(ev, Ev::Gamepad { .. }) { crate::gamepad::handle(*ev); return Completion::Completed; }
                        if self.pointer.is_none() { return Completion::Failed; }
                        self.step_result = Completion::Completed;
                        self.handle(*ev);
                        return self.step_result;
                    }
                }
                Completion::Completed
            },
            Operation::Down(physical) | Operation::Repeat(physical) | Operation::Up(physical) => {
                let down = !matches!(operation, Operation::Up(_));
                let repeat = matches!(operation, Operation::Repeat(_));
                let (device, code) = match *physical {
                    Physical::Key(k) => {
                        let Some(code) = keymap::to_evdev(k) else { return Completion::Unsupported; };
                        (&mut self.keyboard, code)
                    }
                    Physical::Button(b) if b < 3 => (&mut self.pointer, [BTN_LEFT, BTN_MIDDLE, BTN_RIGHT][b as usize]),
                    _ => return Completion::Unsupported,
                };
                let Some(device) = device.as_mut() else { return Completion::Failed; };
                // emit adds SYN_REPORT. An error can follow a partial write: no prefix
                // count is available, so never claim Failed/no effect or retry it.
                let value = if repeat && matches!(physical, Physical::Key(_)) { 2 } else { down as i32 };
                if device.emit(&[InputEvent::new(EventType::KEY.0, code, value)]).is_err() { return Completion::Unknown; }
                match *physical {
                    Physical::Key(_) => { if down { self.held.insert(code); } else { self.held.remove(&code); } }
                    Physical::Button(b) => self.buttons[b as usize] = down,
                }
                Completion::Completed
            }
            Operation::Scalar(ch) => {
                // Choose the US-layout adapter before any input, not after Unicode failure.
                // Only transient keys are released; existing physical keys stay owned.
                let Some((payload, protected)) = fallback_packets(*ch, |k| keymap::to_evdev(k).is_some_and(|code| self.held.contains(&code))) else {
                    return Completion::Unsupported;
                };
                let modifiers: Vec<u16> = (0x36..=0x3e).filter(|&k| k != 0x39)
                    .filter(|&k| keymap::to_evdev(k).is_some_and(|code| self.held.contains(&code))).collect();
                let Some(device) = self.keyboard.as_mut() else { return Completion::Failed; };
                transaction(&modifiers, false, &payload, &protected, |packets| {
                    let events: Option<Vec<_>> = packets.iter().map(|p| match *p {
                        Packet::Key(k, down) => keymap::to_evdev(k).map(|code| InputEvent::new(EventType::KEY.0, code,
                            if down && protected.contains(&k) { 2 } else { down as i32 })),
                        _ => None,
                    }).collect();
                    let events = events?;
                    device.emit(&events).ok().map(|_| packets.len())
                })
            }
        }
    }

    fn reset_state(&mut self) -> crate::text_input::Completion {
        self.step_result = crate::text_input::Completion::Completed;
        self.release_all();
        self.step_result
    }
    fn handle(&mut self, ev: glass_proto::InputEvent) {
        Injector::handle(self, ev)
    }
    fn paste(&mut self) {
        Injector::paste(self)
    }
    fn release_all(&mut self) {
        Injector::release_all(self)
    }
    fn set_displays(&mut self, displays: &[u32]) {
        Injector::set_displays(self, displays)
    }
    fn move_on_display(&mut self, display: u32, x: f32, y: f32) {
        self.ensure_devices();
        self.layout = video::layout_rect();
        if let Some(r) = video::rect_of(display) {
            self.move_in(r, x, y);
        }
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;
    use crate::input_queue::{Control, Operation, Physical};
    use crate::media::InputSink;
    use crate::text_input::Completion;

    fn unavailable() -> Injector {
        Injector {
            step_result: crate::text_input::Completion::Completed,
            keyboard: None, pointer: None, bounds: Vec::new(), layout: Rect::default(),
            held: HashSet::from([KEY_LEFTCTRL]), buttons: [true, false, false],
            wheel: (0.0, 0.0), hi_res: (0.0, 0.0), pos: (0.0, 0.0),
        }
    }
    #[test]
    fn configuration_does_not_reset_native_holds_behind_scheduler() {
        let mut input = unavailable();
        for control in [Control::Window(None)] {
            assert_eq!(input.execute_step(&Operation::Control(control)), Completion::Completed);
            assert!(input.held.contains(&KEY_LEFTCTRL));
            assert!(input.buttons[0]);
        }
    }
    #[test]
    fn unavailable_device_is_failure_not_unsupported_or_completed() {
        let mut input = unavailable();
        assert_eq!(input.execute_step(&Operation::Down(Physical::Key(0))), Completion::Failed);
        assert_eq!(input.execute_step(&Operation::Up(Physical::Button(0))), Completion::Failed);
        assert_eq!(input.execute_step(&Operation::Scalar('a')), Completion::Failed);
        assert_eq!(input.execute_step(&Operation::Scalar('😀')), Completion::Unsupported);
        assert_eq!(input.execute_step(&Operation::Ownership), Completion::Completed);
        assert_eq!(input.held, HashSet::from([KEY_LEFTCTRL]));
        assert!(input.buttons[0]);
    }
}
