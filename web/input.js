// Pointer, touch, keyboard and clipboard input → remote input events.

import { Input, penFields, textEvents } from './proto.js';
import { MAC_KEYCODES, charToKey, KVK_SHIFT } from './keymap.js';
import { typedKey } from './typing.js';
import { log } from './log.js';
import { pointsPerPx, scrollBatcher, wheelPixels } from './scroll.js';

const LONG_PRESS_MS = 500;
const TAP_SLOP_PX = 10;
// Trackpad mode: a tap is shorter than this, a second tap this soon (and close) can drag.
const TAP_MAX_MS = 250;
const DOUBLE_TAP_MS = 300;
const DOUBLE_TAP_SLOP_PX = 40;
// Cursor speed: finger movement × gain × (1 + min(max, speed × accel)).
const TRACKPAD_GAIN = 1.25;
const TRACKPAD_ACCEL = 1.1;
const TRACKPAD_ACCEL_MAX = 2.2;
// A single touch waits this long before pressing, so a second finger can turn
// the gesture into a scroll without a stray click.
const TOUCH_PRESS_DELAY_MS = 120;
const CLIPBOARD_WAIT_MS = 1500;
const MAX_ZOOM = 5;
// A two-finger gesture becomes a pinch once the finger spread changes this much.
const PINCH_THRESHOLD = 0.08;
const PASTE_FALLBACK_MS = 150;

const MODIFIER_CODES = ['MetaLeft', 'MetaRight', 'ShiftLeft', 'ShiftRight', 'AltLeft', 'AltRight', 'ControlLeft', 'ControlRight'];
const MODIFIER_KEYCODES = new Set(MODIFIER_CODES.map((c) => MAC_KEYCODES[c]));

/**
 * @param canvas      the video canvas
 * @param keySink     hidden textarea that holds keyboard focus (and summons the iPad keyboard)
 * @param send        (kind, fields) => void, sends one input event
 * @param control     (msg) => void, sends a control message
 * @param nextClipboard () => Promise<{ text, html?, image }>, the next Mac clipboard
 * @param onMacCopy   (clip) => void, after ⌘C/⌘X: put its rich text or image here too
 * @param onPaste     ({ text, html?, image? }) => void, this device's clipboard, to paste on the Mac
 * @param onLocalShortcut (code) => void, for Ctrl+Shift+<key> shortcuts handled locally
 */
export function attachInput({
  screens, keySink, send: sendNow, control, nextClipboard, onMacCopy = () => {}, onPaste, onLocalShortcut, swapCmdOption = false,
  ctrlAsCmd = true, pcKeyboard = false, onZoom, onResize = () => {},
  macViewer = /Mac|iPhone|iPad/.test(navigator.platform), physicalKeyboard = false, optionAsAlt = false,
}) {
  // With iPadOS set to turn the physical ⌘ key into ⌥ (so it can't claim ⌘Tab etc.),
  // swap them back here so the Mac gets the keys the user actually pressed.
  let swap = swapCmdOption;
  const SWAPPED = {
    MetaLeft: 'AltLeft', MetaRight: 'AltRight', AltLeft: 'MetaLeft', AltRight: 'MetaRight',
  };
  // iPadOS claims these ⌘ shortcuts before any page sees them, so Ctrl+<key> stands in:
  // the Mac receives ⌘ for the rest of that chord (holding Ctrl and tapping Tab cycles apps).
  let remapCtrl = ctrlAsCmd;
  let remappedCtrl; // the Ctrl keycode currently standing in for ⌘, if any
  const CMD = MAC_KEYCODES.MetaLeft;
  const CTRL_KEYCODES = [MAC_KEYCODES.ControlLeft, MAC_KEYCODES.ControlRight];
  const stolenByIPad = (code, shift) =>
    code === 'Tab' || code === 'Space' || (code === 'KeyH' && !shift) ||
    (shift && ['Digit3', 'Digit4', 'Digit5'].includes(code));

  // PC keyboards (Linux, Windows): Ctrl is the everyday shortcut key, so it becomes ⌘ on
  // the Mac, and Super/Windows becomes Ctrl. Alt stays ⌥.
  let pc = pcKeyboard;
  const PC = {
    ControlLeft: 'MetaLeft', ControlRight: 'MetaRight', MetaLeft: 'ControlLeft', MetaRight: 'ControlRight',
  };

  // Remaps exist to make a non-Mac keyboard behave on a Mac; other hosts get keys as typed.
  let macHost = true;
  let textHost = true;
  let physical = physicalKeyboard;
  let optionAlt = optionAsAlt;
  let viewerTyping = false;
  const characterMode = () => textHost && viewerTyping && !physical;
  const touchKeyboard = /iPhone|iPad|Android/.test(navigator.userAgent) ||
    (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
  const codeOf = (e) => (!macHost ? undefined : pc ? PC[e.code] : swap ? SWAPPED[e.code] : undefined) ?? e.code;
  const cmdHeld = (e) => (!macHost ? e.metaKey : pc ? e.ctrlKey : swap ? e.altKey : e.metaKey);
  // Copy and paste: ⌘ on a Mac host; Ctrl on Windows and Linux (and ⌘V still pastes this
  // device's clipboard there, as a Mac viewer expects).
  const copyHeld = (e) => (macHost ? cmdHeld(e) : e.ctrlKey && !e.altKey && !e.metaKey);
  const pasteHeld = (e) => (macHost ? cmdHeld(e) && !swap : e.metaKey || (e.ctrlKey && !e.altKey));
  // Each canvas shows one host stream (data-stream) and is letterboxed by
  // `object-fit: contain`; pointer positions map into the video's content box. Rects are
  // cached and dropped on resize, so pointer events never force a layout read.
  const videoSizes = []; // stream -> { width, height }
  const rects = new Map(); // canvas -> content rect
  const resize = new ResizeObserver(() => {
    rects.clear();
    drawCursor();
    onResize();
  });
  const streamOf = (canvas) => Number(canvas.dataset.stream);
  // A display is a canvas (WebTransport) or a <video> (remote, over WebRTC).
  const canvasOf = (e) => e.target?.closest?.('#screens > [data-stream]') ?? undefined;

  function contentRect(canvas) {
    let rect = rects.get(canvas);
    const size = videoSizes[streamOf(canvas)];
    if (rect || !size) return rect;
    const r = canvas.getBoundingClientRect();
    const scale = Math.min(r.width / size.width, r.height / size.height);
    const w = size.width * scale;
    const h = size.height * scale;
    rect = { left: r.left + (r.width - w) / 2, top: r.top + (r.height - h) / 2, width: w, height: h };
    rects.set(canvas, rect);
    return rect;
  }

  // Pointer lock (Capture mouse): relative motion only, the local position means nothing.
  let locked = false;

  // Scrolling goes in the host's desktop points (what a display's `points_width` counts),
  // so content moves as far as the fingers did whatever size it's shown at. One send a frame.
  const pointsWidths = []; // stream -> width in desktop points
  const scrolls = scrollBatcher((a, b) => sendNow(Input.SCROLL, { a, b }));
  function scrollBy(dx, dy, canvas) {
    flushModifiers();
    const k = canvas ? pointsPerPx(pointsWidths[streamOf(canvas)] ?? videoSizes[streamOf(canvas)]?.width, contentRect(canvas)?.width) : 1;
    scrolls.add(dx * k, dy * k);
  }
  // Moves don't wait for a pending scroll; clicks and keys go after it.
  const UNBATCHED = new Set([Input.MOVE, Input.MOVE_BY, Input.PEN]);
  const pointerButtons = new Set();
  const send = (kind, fields) => {
    // A Shift-click/Alt-drag needs modifiers even though no physical key followed them.
    if (kind === Input.DOWN || kind === Input.UP) flushModifiers();
    if (kind === Input.DOWN) pointerButtons.add(fields.button);
    if (kind === Input.UP) pointerButtons.delete(fields.button);
    if (!UNBATCHED.has(kind)) scrolls.flush();
    sendNow(kind, fields);
  };

  function sendMove(x, y, canvas) {
    if (locked) return;
    const rect = canvas && contentRect(canvas);
    if (!rect) return;
    send(Input.MOVE, { button: streamOf(canvas), a: (x - rect.left) / rect.width, b: (y - rect.top) / rect.height });
  }

  function focusKeys() {
    keySink.focus({ preventScroll: true });
  }

  // ---- Pen (Apple Pencil, stylus): pressure and tilt ride ahead of each mouse event ----

  let penNear = false;
  let lastPen = '';
  /** Tilt in degrees toward +x and +y, from tiltX/Y or (Safari) altitude and azimuth. */
  function tiltOf(e) {
    if (e.tiltX || e.tiltY || e.altitudeAngle === undefined) return [e.tiltX ?? 0, e.tiltY ?? 0];
    // Upright (altitude π/2) is no tilt.
    const t = Math.tan(e.altitudeAngle);
    if (!t || t > 1e6) return [0, 0];
    const deg = (r) => (r * 180) / Math.PI;
    return [deg(Math.atan(Math.cos(e.azimuthAngle) / t)), deg(Math.atan(Math.sin(e.azimuthAngle) / t))];
  }
  /** Sends the pen's state if it changed; `e` null when it leaves. */
  function sendPen(e) {
    const near = Boolean(e);
    let fields;
    if (near) {
      const [tiltX, tiltY] = tiltOf(e);
      // Hovering (Pencil hover, M2 iPads) touches nothing: no pressure.
      const pressure = e.buttons & 1 ? e.pressure : 0;
      fields = penFields({ near, pressure, tiltX, tiltY, twist: e.twist ?? 0 });
    } else {
      if (!penNear) return;
      fields = penFields({ near: false });
    }
    penNear = near;
    const key = `${fields.button}|${fields.keycode}|${fields.a.toFixed(3)}|${Math.round(fields.b)}`;
    if (key === lastPen) return;
    lastPen = key;
    send(Input.PEN, fields);
  }
  /** A non-pen pointer took over (mouse, finger): the pen isn't near any more. */
  function penGone(e) {
    if (penNear && e.pointerType !== 'pen') sendPen(null);
  }

  // ---- Mouse, trackpad and pen: direct mapping ----

  const moveEvent = 'onpointerrawupdate' in window ? 'pointerrawupdate' : 'pointermove';
  screens.addEventListener(moveEvent, (e) => {
    if (locked) {
      if (e.movementX || e.movementY) send(Input.MOVE_BY, { a: e.movementX, b: e.movementY });
      return;
    }
    penGone(e);
    if (e.pointerType === 'pen') {
      // Every sample the pen took since the last event, for smooth strokes.
      const canvas = canvasOf(e);
      // (pointerrawupdate has none: it is already every sample.)
      const samples = e.getCoalescedEvents?.() ?? [];
      for (const c of samples.length ? samples : [e]) {
        sendPen(c);
        sendMove(c.clientX, c.clientY, canvas);
      }
      return;
    }
    if (e.pointerType !== 'touch') sendMove(e.clientX, e.clientY, canvasOf(e));
  }, { passive: true });
  // The pen lifted away from the screen.
  screens.addEventListener('pointerleave', (e) => {
    if (e.pointerType === 'pen') sendPen(null);
  });

  screens.addEventListener('pointerdown', (e) => {
    const canvas = canvasOf(e);
    if (!canvas) return;
    e.preventDefault();
    if (e.pointerType === 'touch') return trackpad ? tpDown(e, canvas) : touchDown(e, canvas);
    // Desktop dead keys/IME need an editable target before composition starts.
    if (characterMode() && !touchKeyboard) focusKeys();
    // iPad Safari leaves fullscreen when a text field takes focus; its hardware keys
    // reach the window without it. Its keyboard button still focuses the sink explicitly.
    if (!locked) try { canvas.setPointerCapture(e.pointerId); } catch {}
    penGone(e);
    if (e.pointerType === 'pen') sendPen(e);
    sendMove(e.clientX, e.clientY, canvas);
    send(Input.DOWN, { button: e.button });
  });
  screens.addEventListener('pointerup', (e) => {
    e.preventDefault();
    if (e.pointerType === 'touch') return trackpad ? tpUp(e) : touchUp(e);
    if (e.pointerType === 'pen') sendPen(e);
    sendMove(e.clientX, e.clientY, canvasOf(e));
    send(Input.UP, { button: e.button });
  });
  screens.addEventListener('pointercancel', (e) => {
    if (e.pointerType === 'touch') (trackpad ? tpUp : touchUp)(e, true);
    if (e.pointerType === 'pen') sendPen(null);
  });
  screens.addEventListener('contextmenu', (e) => e.preventDefault());
  screens.addEventListener('wheel', (e) => {
    const canvas = canvasOf(e);
    // Point at the screen being scrolled first, so the scroll lands on that display.
    sendMove(e.clientX, e.clientY, canvas);
    const { dx, dy } = wheelPixels(e);
    scrollBy(dx, dy, canvas);
    e.preventDefault();
  }, { passive: false });

  // ---- Touch: tap = click, drag = drag, long-press = right-click, two fingers = scroll ----

  const touches = new Map(); // pointerId -> { x, y }
  let gesture; // { startX, startY, pressed, scrolling, done, pressTimer, longTimer }

  function centroid() {
    let x = 0;
    let y = 0;
    for (const t of touches.values()) { x += t.x; y += t.y; }
    return { x: x / touches.size, y: y / touches.size };
  }

  function clearTimers() {
    clearTimeout(gesture?.pressTimer);
    clearTimeout(gesture?.longTimer);
  }

  function touchDown(e, canvas) {
    // Capture can throw (e.g. a pointer the browser already considers gone); tracking the
    // gesture matters more than capture.
    try { canvas.setPointerCapture(e.pointerId); } catch {}
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (touches.size === 1) {
      sendMove(e.clientX, e.clientY, canvas);
      gesture = { canvas, startX: e.clientX, startY: e.clientY, pressed: false, scrolling: false, done: false };
      gesture.pressTimer = setTimeout(() => {
        if (!gesture.scrolling && !gesture.done && touches.size === 1) {
          gesture.pressed = true;
          send(Input.DOWN, { button: 0 });
        }
      }, TOUCH_PRESS_DELAY_MS);
      gesture.longTimer = setTimeout(() => {
        if (gesture.scrolling || gesture.done || touches.size !== 1) return;
        // Long press without moving: right-click instead of the pending left press.
        if (gesture.pressed) send(Input.UP, { button: 0 });
        send(Input.DOWN, { button: 2 });
        send(Input.UP, { button: 2 });
        gesture.done = true;
      }, LONG_PRESS_MS);
    } else if (gesture && !gesture.done) {
      clearTimers();
      if (gesture.pressed) send(Input.UP, { button: 0 });
      gesture.pressed = false;
      gesture.scrolling = true;
      gesture.last = centroid();
      // Decided on first movement: pinch (zoom), pan (when zoomed) or scroll the remote.
      gesture.mode = undefined;
      gesture.start = { c: centroid(), spread: spread(), zoom: { ...zoom } };
    }
  }

  // ---- Local zoom (pinch): scales the remote screens on this device only ----

  const zoom = { s: 1, x: 0, y: 0 };

  function spread() {
    const [a, b] = [...touches.values()];
    return b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
  }

  /** Scale `s` around the fingers: the point that was under `c0` ends up under `c`. */
  function zoomTo(s, c, start) {
    s = Math.min(MAX_ZOOM, Math.max(1, s));
    const px = (start.c.x - start.zoom.x) / start.zoom.s;
    const py = (start.c.y - start.zoom.y) / start.zoom.s;
    const w = screens.clientWidth;
    const h = screens.clientHeight;
    // Keep the screens covering the viewport: no panning past the edges.
    zoom.s = s;
    zoom.x = Math.min(0, Math.max(w - w * s, c.x - px * s));
    zoom.y = Math.min(0, Math.max(h - h * s, c.y - py * s));
    applyZoom();
  }

  function applyZoom() {
    screens.style.transform = zoom.s === 1 ? '' : `translate(${zoom.x}px, ${zoom.y}px) scale(${zoom.s})`;
    rects.clear();
    onZoom?.(zoom.s);
    drawCursor();
  }

  function resetZoom() {
    Object.assign(zoom, { s: 1, x: 0, y: 0 });
    applyZoom();
  }

  screens.addEventListener('pointermove', (e) => {
    if (trackpad && e.pointerType === 'touch') return tpMove(e);
    if (e.pointerType !== 'touch' || !touches.has(e.pointerId) || !gesture) return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (gesture.scrolling) {
      const c = centroid();
      const start = gesture.start;
      if (!gesture.mode && start && touches.size >= 2) {
        const ratio = spread() / (start.spread || 1);
        const moved = Math.hypot(c.x - start.c.x, c.y - start.c.y) > TAP_SLOP_PX;
        if (Math.abs(ratio - 1) > PINCH_THRESHOLD) gesture.mode = 'zoom';
        else if (moved) gesture.mode = zoom.s > 1 ? 'zoom' : 'scroll';
      }
      if (gesture.mode === 'zoom' && touches.size >= 2) {
        zoomTo(start.zoom.s * (spread() / (start.spread || 1)), c, start);
      } else if (gesture.mode === 'scroll') {
        // Natural scrolling: content follows the fingers.
        scrollBy(gesture.last.x - c.x, gesture.last.y - c.y, gesture.canvas);
      }
      gesture.last = c;
      return;
    }
    if (gesture.done) return;
    const moved = Math.hypot(e.clientX - gesture.startX, e.clientY - gesture.startY) > TAP_SLOP_PX;
    if (moved && !gesture.pressed) {
      // Started dragging before the press delay elapsed: press now.
      clearTimers();
      gesture.pressed = true;
      send(Input.DOWN, { button: 0 });
    } else if (moved) {
      clearTimeout(gesture.longTimer);
    }
    sendMove(e.clientX, e.clientY, gesture.canvas);
  }, { passive: true });

  function touchUp(e, cancelled = false) {
    touches.delete(e.pointerId);
    // Snap back to 1× when a pinch ends just above it.
    if (touches.size === 0 && gesture?.mode === 'zoom' && zoom.s < 1.05) resetZoom();
    if (!gesture || touches.size > 0) return;
    clearTimers();
    if (!gesture.done && !gesture.scrolling && !cancelled) {
      if (!gesture.pressed) send(Input.DOWN, { button: 0 });
      send(Input.UP, { button: 0 });
    } else if (gesture.pressed && !gesture.done) {
      send(Input.UP, { button: 0 });
    }
    gesture = undefined;
  }

  // ---- Trackpad mode: the screen is a laptop trackpad for a cursor ----
  //
  // One finger moves the cursor (relative, with acceleration); tap clicks; double-tap and
  // hold, or a long press, drags; two-finger tap right-clicks; two fingers scroll or pinch
  // to zoom, and a zoomed view follows the cursor. The cursor is drawn here at once (the
  // video's own follows a moment later), so moving it never feels behind.

  let trackpad = false;
  const cursorEl = document.getElementById('tp-cursor');
  const cursor = { x: NaN, y: NaN }; // in the screens' own (unzoomed) coordinates
  let tp; // the gesture in progress
  let lastTap; // { t, x, y }: a second tap soon after, held, drags
  let cursorFrame;

  const toClient = (p) => ({ x: zoom.x + p.x * zoom.s, y: zoom.y + p.y * zoom.s });
  const fromClient = (c) => ({ x: (c.x - zoom.x) / zoom.s, y: (c.y - zoom.y) / zoom.s });
  const shown = () => [...screens.querySelectorAll('[data-stream]:not(.hidden)')];

  /** Each visible display's picture, in unzoomed coordinates. */
  function areasNow() {
    return shown().flatMap((canvas) => {
      const r = contentRect(canvas);
      if (!r) return [];
      const a = fromClient({ x: r.left, y: r.top });
      return [{ canvas, x: a.x, y: a.y, w: r.width / zoom.s, h: r.height / zoom.s }];
    });
  }

  /** The cursor kept on a picture: the one it's over, else the nearest. */
  function placeCursor() {
    const areas = areasNow();
    if (!areas.length) return undefined;
    if (Number.isNaN(cursor.x)) {
      cursor.x = areas[0].x + areas[0].w / 2;
      cursor.y = areas[0].y + areas[0].h / 2;
    }
    let best;
    let bestD = Infinity;
    for (const a of areas) {
      const x = Math.min(a.x + a.w - 1, Math.max(a.x, cursor.x));
      const y = Math.min(a.y + a.h - 1, Math.max(a.y, cursor.y));
      const d = Math.hypot(x - cursor.x, y - cursor.y);
      if (d < bestD) { bestD = d; best = { a, x, y }; }
    }
    cursor.x = best.x;
    cursor.y = best.y;
    return best.a.canvas;
  }

  /** Sends the cursor's position; returns the picture it's on. */
  function sendCursor() {
    const canvas = placeCursor();
    if (!canvas) return undefined;
    const c = toClient(cursor);
    sendMove(c.x, c.y, canvas);
    return canvas;
  }

  function drawCursor() {
    if (!trackpad || Number.isNaN(cursor.x) || cursorFrame) return;
    cursorFrame = requestAnimationFrame(() => {
      cursorFrame = undefined;
      const c = toClient(cursor);
      cursorEl.style.transform = `translate(${c.x}px, ${c.y}px)`;
    });
  }

  /** Zoomed in: pan so the cursor stays comfortably inside the view. */
  function follow() {
    if (zoom.s <= 1) return;
    const w = screens.clientWidth;
    const h = screens.clientHeight;
    const m = Math.min(90, w * 0.18, h * 0.18);
    const c = toClient(cursor);
    let { x, y } = zoom;
    if (c.x < m) x += m - c.x;
    else if (c.x > w - m) x -= c.x - (w - m);
    if (c.y < m) y += m - c.y;
    else if (c.y > h - m) y -= c.y - (h - m);
    x = Math.min(0, Math.max(w - w * zoom.s, x));
    y = Math.min(0, Math.max(h - h * zoom.s, y));
    if (x !== zoom.x || y !== zoom.y) {
      zoom.x = x;
      zoom.y = y;
      applyZoom();
    }
  }

  function moveCursorBy(dx, dy) {
    placeCursor();
    cursor.x += dx / zoom.s;
    cursor.y += dy / zoom.s;
    sendCursor();
    follow();
    drawCursor();
  }

  function tpClick(button) {
    sendCursor();
    send(Input.DOWN, { button });
    send(Input.UP, { button });
  }

  function tpStartDrag() {
    if (!tp || tp.dragging) return;
    clearTimeout(tp.longTimer);
    tp.dragging = true;
    sendCursor();
    send(Input.DOWN, { button: 0 });
    cursorEl.classList.add('down');
  }

  function tpDown(e, canvas) {
    try { canvas.setPointerCapture(e.pointerId); } catch {}
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const now = performance.now();
    if (touches.size === 1) {
      placeCursor();
      drawCursor();
      const again = lastTap && now - lastTap.t < DOUBLE_TAP_MS && Math.hypot(e.clientX - lastTap.x, e.clientY - lastTap.y) < DOUBLE_TAP_SLOP_PX;
      tp = { startX: e.clientX, startY: e.clientY, lastX: e.clientX, lastY: e.clientY, lastT: now, startT: now, moved: false, dragging: false, multi: false, again };
      // Hold still: press and hold, to drag.
      tp.longTimer = setTimeout(() => { if (tp && !tp.moved && !tp.multi) tpStartDrag(); }, LONG_PRESS_MS);
      return;
    }
    if (!tp) return;
    clearTimeout(tp.longTimer);
    tp.multi = true;
    tp.two = { c: centroid(), spread: spread(), zoom: { ...zoom }, last: centroid(), mode: undefined };
  }

  function tpMove(e) {
    if (!tp || !touches.has(e.pointerId)) return;
    touches.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (tp.two && touches.size >= 2) {
      const c = centroid();
      const two = tp.two;
      if (!two.mode) {
        const ratio = spread() / (two.spread || 1);
        if (Math.abs(ratio - 1) > PINCH_THRESHOLD) two.mode = 'zoom';
        else if (Math.hypot(c.x - two.c.x, c.y - two.c.y) > TAP_SLOP_PX) two.mode = 'scroll';
      }
      if (two.mode === 'zoom') {
        zoomTo(two.zoom.s * (spread() / (two.spread || 1)), c, two);
      } else if (two.mode === 'scroll') {
        scrollBy(two.last.x - c.x, two.last.y - c.y, sendCursor());
      }
      two.last = c;
      return;
    }
    if (tp.multi) return; // a finger left over from a two-finger gesture
    const now = performance.now();
    if (!tp.moved && Math.hypot(e.clientX - tp.startX, e.clientY - tp.startY) > TAP_SLOP_PX) {
      tp.moved = true;
      clearTimeout(tp.longTimer);
      if (tp.again) tpStartDrag(); // double-tap, hold and move: drag
    }
    if (!tp.moved) return;
    // Acceleration: slow and precise when the finger is slow, quick when it flicks.
    const dx = e.clientX - tp.lastX;
    const dy = e.clientY - tp.lastY;
    const dt = Math.max(1, now - tp.lastT);
    const speed = Math.hypot(dx, dy) / dt; // px per ms
    const gain = TRACKPAD_GAIN * (1 + Math.min(TRACKPAD_ACCEL_MAX, speed * TRACKPAD_ACCEL));
    tp.lastX = e.clientX;
    tp.lastY = e.clientY;
    tp.lastT = now;
    moveCursorBy(dx * gain, dy * gain);
  }

  function tpUp(e, cancelled = false) {
    touches.delete(e.pointerId);
    if (!tp || touches.size > 0) return;
    clearTimeout(tp.longTimer);
    const now = performance.now();
    if (tp.dragging) {
      sendCursor();
      send(Input.UP, { button: 0 });
      cursorEl.classList.remove('down');
    } else if (!cancelled && !tp.multi && !tp.moved && now - tp.startT < TAP_MAX_MS) {
      tpClick(0);
      lastTap = { t: now, x: e.clientX, y: e.clientY };
    } else if (!cancelled && tp.multi && !tp.two?.mode && now - tp.startT < TAP_MAX_MS * 1.5) {
      tpClick(2); // two-finger tap
    }
    if (tp.two?.mode === 'zoom' && zoom.s < 1.05) resetZoom();
    tp = undefined;
  }

  function setTrackpad(on) {
    trackpad = on;
    tp = undefined;
    touches.clear();
    cursorEl.classList.toggle('hidden', !on);
    if (on) {
      placeCursor();
      drawCursor();
    }
  }

  // ---- Keyboard ----

  const heldKeys = new Set();
  // Locally held modifiers are sent only when a physical action needs them. The boolean
  // records use, so an Option consumed by text never becomes a bare Alt tap on release.
  const typingModifiers = new Map();
  const textKeys = new Set();
  let composing = false;
  function flushModifiers() {
    for (const keycode of typingModifiers.keys()) {
      typingModifiers.set(keycode, true);
      if (keycode === remappedCtrl) continue;
      if (!heldKeys.has(keycode)) {
        heldKeys.add(keycode);
        send(Input.KEY_DOWN, { keycode });
      }
    }
  }
  function prepareText() {
    for (const keycode of typingModifiers.keys()) typingModifiers.set(keycode, true);
    // A modifier already used by a physical action remains held until its real keyup.
    // Native text injection suspends Glass-owned modifiers; releasing Alt here can open menus.
  }
  function resetKeyboard() {
    releaseKeys();
    typingModifiers.clear();
    textKeys.clear();
    composing = false;
    composed = undefined;
    keySink.value = '';
    remappedCtrl = undefined;
  }
  let pasteHandled = true;
  // Safari fires `paste` only into a focused text field, so ⌘ going down focuses the hidden
  // one until ⌘ is released: ⌘V then pastes this device's clipboard rather than the Mac's own.
  let focusedForPaste = false;
  function unfocusAfterPaste() {
    if (!focusedForPaste) return;
    focusedForPaste = false;
    if (document.activeElement === keySink) keySink.blur();
  }
  function releaseKeys(filter = () => true) {
    for (const keycode of heldKeys) {
      if (filter(keycode)) {
        send(Input.KEY_UP, { keycode });
        heldKeys.delete(keycode);
      }
    }
  }

  function tapKey(keycode, shift) {
    if (shift) send(Input.KEY_DOWN, { keycode: KVK_SHIFT });
    send(Input.KEY_DOWN, { keycode });
    send(Input.KEY_UP, { keycode });
    if (shift) send(Input.KEY_UP, { keycode: KVK_SHIFT });
  }

  /**
   * Committed characters (IME/on-screen keyboard) are already text, independent of
   * hardware-key routing or viewerTyping support. Linux retains its legacy US-key fallback.
   */
  function typeText(text) {
    if (!text) return;
    if (textHost) {
      prepareText();
      for (const bytes of textEvents(text)) send(Input.TEXT, { bytes });
      return;
    }
    const keys = [...text].map(charToKey);
    if (keys.every(Boolean)) {
      for (const key of keys) tapKey(key.keycode, key.shift);
      return;
    }
    for (const bytes of textEvents(text)) send(Input.TEXT, { bytes });
  }

  // Typing in Glass's own fields (token prompt, share link) stays local.
  const localField = (el) => el !== keySink && el?.matches?.('input, textarea');

  window.addEventListener('keydown', (e) => {
    if (localField(e.target)) return;
    if (e.metaKey && document.activeElement !== keySink) {
      keySink.focus({ preventScroll: true });
      focusedForPaste = document.activeElement === keySink;
    }
    if (e.ctrlKey && e.shiftKey && !e.metaKey && !e.altKey && ['KeyF', 'KeyH'].includes(e.code)) {
      e.preventDefault();
      onLocalShortcut(e.code);
      return;
    }
    // Do not cancel the browser's dead-key/IME processing. Only its commit is transmitted.
    if (composing || e.isComposing || e.keyCode === 229 || (characterMode() && e.key === 'Dead' && !(!macHost && macViewer && optionAlt && e.altKey))) {
      if (characterMode()) prepareText();
      return;
    }
    const code = codeOf(e);
    const keycode = MAC_KEYCODES[code];
    if (characterMode()) {
      if (MODIFIER_KEYCODES.has(keycode)) {
        if (!typingModifiers.has(keycode)) typingModifiers.set(keycode, false);
        // A modifier pressed after mouse-down must affect the ongoing drag immediately.
        const physicalHold = [...heldKeys].some(k => !MODIFIER_KEYCODES.has(k));
        const characterAlt = (e.code === 'AltLeft' || e.code === 'AltRight') &&
          !(optionAlt && !macHost) && (macViewer || e.getModifierState?.('AltGraph'));
        if (pointerButtons.size || (physicalHold && !characterAlt)) flushModifiers();
        return;
      }
      // An already transmitted physical key keeps its route for the entire hold.
      const text = !heldKeys.has(keycode) && typedKey(e, { macViewer, commandHeld: cmdHeld(e), optionAsAlt: !macHost && optionAlt });
      const physicalSpace = code === 'Space' && text === ' ' && !e.altKey && !e.getModifierState?.('AltGraph');
      if (text && !physicalSpace) {
        e.preventDefault();
        if (e.repeat && !textKeys.has(e.code)) return;
        if (!e.repeat && textKeys.has(e.code)) return;
        textKeys.add(e.code);
        typeText(text);
        return;
      }
      if (textKeys.has(e.code)) { e.preventDefault(); return; }
    }
    // On-screen keyboards often report no code; their text arrives via beforeinput.
    if (keycode === undefined || (e.repeat && !heldKeys.has(keycode))) return;

    flushModifiers();

    if (pasteHeld(e) && code === 'KeyV') {
      // On Windows and Linux ⌘ is the Windows/Super key, and letting it go alone opens Start
      // (or the launcher); the paste can get there after it does. A key that does nothing
      // with it, pressed now, makes it not alone.
      if (!macHost && e.metaKey) {
        send(Input.KEY_DOWN, { keycode: MAC_KEYCODES.F20 });
        send(Input.KEY_UP, { keycode: MAC_KEYCODES.F20 });
      }
      // Let the browser fire `paste`; the host gets the text and presses Cmd+V itself, so
      // the Mac pastes what's on this device's clipboard. Without a focused text field some
      // browsers never fire it, so fall back to sending ⌘V as a plain keystroke.
      pasteHandled = false;
      setTimeout(() => {
        if (pasteHandled) return;
        log('info', 'paste: no paste event, sent ⌘V as a keystroke', { focused: document.activeElement === keySink });
        send(Input.KEY_DOWN, { keycode });
        send(Input.KEY_UP, { keycode });
      }, PASTE_FALLBACK_MS);
      return;
    }
    if (copyHeld(e) && (code === 'KeyC' || code === 'KeyX') && navigator.clipboard?.write) {
      // Must be called during the keydown for Safari; the item resolves once the
      // Mac reports its new clipboard.
      const clip = nextClipboard(CLIPBOARD_WAIT_MS);
      const text = clip.then((c) => new Blob([c.text], { type: 'text/plain' }));
      navigator.clipboard.write([new ClipboardItem({ 'text/plain': text })]).catch(() => {});
      // Rich text or an image: written again, whole, once it's here.
      clip.then(onMacCopy, () => {});
    }

    e.preventDefault();
    if (macHost && remapCtrl && !pc && !remappedCtrl && e.ctrlKey && !cmdHeld(e) && stolenByIPad(code, e.shiftKey)) {
      // Swap the Ctrl the Mac already sees for ⌘, until Ctrl is released.
      for (const ctrl of CTRL_KEYCODES) {
        if (heldKeys.delete(ctrl)) {
          send(Input.KEY_UP, { keycode: ctrl });
          remappedCtrl = ctrl;
        }
      }
      heldKeys.add(CMD);
      send(Input.KEY_DOWN, { keycode: CMD });
    }
    heldKeys.add(keycode);
    send(Input.KEY_DOWN, { keycode });
  });

  window.addEventListener('keyup', (e) => {
    if (localField(e.target)) return;
    // macOS can swallow character keyups while physical Command is held. Clear their
    // text ownership even if Command itself was deferred and has no transmitted keyup.
    if (e.code === 'MetaLeft' || e.code === 'MetaRight') { textKeys.clear(); }
    if (!e.metaKey) unfocusAfterPaste();
    const code = codeOf(e);
    const keycode = MAC_KEYCODES[code];
    if (textKeys.delete(e.code)) { e.preventDefault(); return; }
    if (keycode === undefined) return;
    e.preventDefault();
    const modifierUsed = typingModifiers.get(keycode);
    typingModifiers.delete(keycode);
    if (keycode === remappedCtrl) {
      remappedCtrl = undefined;
      if (heldKeys.delete(CMD)) send(Input.KEY_UP, { keycode: CMD });
      return;
    }
    if (!heldKeys.delete(keycode)) {
      if (modifierUsed === false) tapKey(keycode, false);
      return;
    }
    send(Input.KEY_UP, { keycode });
    // macOS browsers swallow keyup for keys pressed while Cmd was held.
    if (code === 'MetaLeft' || code === 'MetaRight') releaseKeys((k) => !MODIFIER_KEYCODES.has(k));
  });

  window.addEventListener('blur', () => {
    resetKeyboard();
    unfocusAfterPaste();
  });

  document.addEventListener('paste', (e) => {
    if (localField(e.target)) return;
    e.preventDefault();
    pasteHandled = true;
    const data = e.clipboardData;
    const text = data?.getData('text/plain') ?? '';
    const html = data?.getData('text/html') || undefined;
    const image = [...(data?.files ?? [])].find((f) => f.type.startsWith('image/'));
    if (!text && !html && !image) return;
    log('info', 'paste: sent this device\'s clipboard', { length: text.length, html: Boolean(html), image: image?.size });
    if (onPaste) onPaste({ text, html, image });
    else control({ t: 'clipboard', text, paste: true });
  });

  // An IME (Chinese, Japanese, Korean; Android keyboards for every word) composes in the
  // key sink, which can't be stopped; what it settles on is typed, and the field emptied.
  let composed; // { text, at }: Safari can follow compositionend with the same insertText
  keySink.addEventListener('compositionstart', () => {
    releaseKeys(k => !MODIFIER_KEYCODES.has(k));
    textKeys.clear();
    composing = true;
    if (characterMode()) prepareText();
  });
  keySink.addEventListener('compositionend', (e) => {
    composing = false;
    typeText(e.data);
    composed = { text: e.data, at: performance.now() };
    keySink.value = '';
  });

  // On-screen keyboard text (and anything else that bypassed keydown).
  keySink.addEventListener('beforeinput', (e) => {
    // Composition steps can't be cancelled; compositionend types the result.
    if (e.isComposing || /Composition/.test(e.inputType)) return;
    e.preventDefault();
    switch (e.inputType) {
      case 'insertText':
      case 'insertReplacementText': {
        const text = e.data ?? e.dataTransfer?.getData('text/plain') ?? '';
        const again = composed && composed.text === text && performance.now() - composed.at < 100;
        composed = undefined;
        if (!again) typeText(text);
        break;
      }
      case 'insertLineBreak':
      case 'insertParagraph':
        tapKey(MAC_KEYCODES.Enter, false);
        break;
      case 'deleteContentBackward':
        tapKey(MAC_KEYCODES.Backspace, false);
        break;
      case 'deleteContentForward':
        tapKey(MAC_KEYCODES.Delete, false);
        break;
    }
  });

  return {
    setTrackpad,
    /** A display's picture in client pixels (letterboxed), or undefined before its size is known. */
    contentRect,
    /** The displays changed size or layout: keep the trackpad cursor on a picture. */
    refreshCursor() {
      if (!trackpad) return;
      placeCursor();
      drawCursor();
    },
    setPointerLocked(on) {
      locked = on;
    },
    /** Presses keys in order and releases them in reverse, e.g. [Cmd, Tab]. */
    sendCombo(keycodes) {
      flushModifiers();
      for (const keycode of keycodes) send(Input.KEY_DOWN, { keycode });
      for (const keycode of [...keycodes].reverse()) send(Input.KEY_UP, { keycode });
    },
    /** The streamed displays (index = stream), for their size in desktop points. */
    setStreams(streams) {
      pointsWidths.length = 0;
      streams.forEach((d, i) => { pointsWidths[i] = d.points_width; });
    },
    setVideoSize(stream, width, height) {
      videoSizes[stream] = { width, height };
      rects.clear();
      if (trackpad) {
        placeCursor();
        drawCursor();
      }
    },
    observeCanvas(canvas) {
      resize.observe(canvas);
    },
    focusKeys,
    resetZoom,
    setCtrlAsCmd(on) {
      resetKeyboard();
      remapCtrl = on;
    },
    setViewerTyping(on) {
      resetKeyboard();
      viewerTyping = on === true;
    },
    setHostOs(os) {
      resetKeyboard();
      viewerTyping = false;
      macHost = os === 'macos';
      textHost = os === 'macos' || os === 'windows';
    },
    setOptionAsAlt(on) {
      resetKeyboard();
      optionAlt = on;
    },
    setPhysicalKeyboard(on) {
      resetKeyboard();
      physical = on;
    },
    setPcKeyboard(on) {
      resetKeyboard();
      pc = on;
    },
    setSwapCmdOption(on) {
      resetKeyboard();
      swap = on;
    },
  };
}
