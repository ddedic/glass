// What stays pressed on the host (web/input.js), with just enough of a page to run it.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { setMaxListeners } from 'node:events';
import { Input } from '../../web/proto.js';
import { MAC_KEYCODES as K } from '../../web/keymap.js';

const realInterval = globalThis.setInterval;
globalThis.setInterval = (...a) => realInterval(...a).unref(); // log.js flushes on a timer
const win = new EventTarget();
setMaxListeners(0, win);
globalThis.window = win;
globalThis.addEventListener = win.addEventListener.bind(win);
const doc = new EventTarget();
setMaxListeners(0, doc);
const cursorEl = { classList: { add: (c) => cursorEl.classes.add(c), remove: (c) => cursorEl.classes.delete(c), toggle() {} }, classes: new Set(), style: {} };
doc.getElementById = () => cursorEl;
doc.activeElement = undefined;
globalThis.document = doc;
globalThis.location = { hash: '', href: 'https://x/' };
globalThis.screen = { width: 1, height: 1 };
globalThis.devicePixelRatio = 1;
globalThis.ResizeObserver = class { observe() {} };
globalThis.requestAnimationFrame = () => 1;
globalThis.fetch = async () => ({});
const { attachInput } = await import('../../web/input.js');

function setup(options = {}) {
  const canvas = {
    dataset: { stream: '0' },
    setPointerCapture() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 100 }),
  };
  const screens = new EventTarget();
  screens.closest = () => canvas;
  screens.querySelectorAll = () => [canvas];
  screens.contains = (el) => el === screens || el === canvas;
  screens.style = {};
  const keySink = Object.assign(new EventTarget(), { focus() {}, blur() {} });
  const sent = [];
  const input = attachInput({
    screens, keySink, control() {}, nextClipboard: async () => ({}), onPaste: (c) => sent.push(['paste', c.text]),
    onLocalShortcut() {}, onZoom() {}, send: (kind, f) => sent.push([kind, f.button ?? f.keycode]),
    ...options,
  });
  input.setVideoSize(0, 100, 100);
  const pointer = (target, type, fields = {}) => target.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), { clientX: 10, clientY: 10, pointerId: 1, button: 0, pointerType: 'mouse', ...fields }));
  const key = (type, code, fields = {}) => win.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), { code, ctrlKey: false, metaKey: false, shiftKey: false, altKey: false, ...fields }));
  const of = (kind) => sent.filter(([k]) => k === kind).map(([, v]) => v);
  return { input, screens, sent, pointer, key, of };
}

test('a cancelled pen doesn\'t leave the button down', () => {
  const { screens, pointer, of } = setup();
  pointer(screens, 'pointerdown', { pointerType: 'pen' });
  assert.deepEqual(of(Input.DOWN), [0]);
  pointer(screens, 'pointercancel', { pointerType: 'pen' });
  assert.deepEqual(of(Input.UP), [0]);
});

test('every button comes up with the last release', () => {
  const { screens, pointer, of } = setup();
  pointer(screens, 'pointerdown');
  pointer(screens, 'pointerup', { button: 2 }); // a chord ends naming the last button only
  assert.deepEqual(of(Input.UP), [0]);
  pointer(screens, 'pointerup');
  assert.deepEqual(of(Input.UP), [0], 'nothing was down any more');
});

test('leaving the window lets go of the mouse too', () => {
  const { screens, pointer, of } = setup();
  pointer(screens, 'pointerdown');
  win.dispatchEvent(new Event('blur'));
  assert.deepEqual(of(Input.UP), [0]);
});

test('a mouse let go away from the screens comes up', () => {
  const { screens, pointer, of } = setup();
  pointer(screens, 'pointerdown');
  pointer(win, 'pointerup');
  assert.deepEqual(of(Input.UP), [0]);
});

test('a touch that ended away from the screens isn\'t a finger still down', () => {
  const { screens, pointer, of } = setup();
  const touch = { pointerType: 'touch', pointerId: 7 };
  pointer(screens, 'pointerdown', touch);
  pointer(win, 'pointerup', touch); // capture failed: it never came back to the screens
  assert.deepEqual(of(Input.UP), []);
  // The next touch is one finger, not a second: a tap, not a two-finger gesture.
  pointer(screens, 'pointerdown', { pointerType: 'touch', pointerId: 8 });
  pointer(screens, 'pointerup', { pointerType: 'touch', pointerId: 8 });
  assert.deepEqual(of(Input.DOWN), [0]);
  assert.deepEqual(of(Input.UP), [0]);
});

test('switching touch modes mid-press lets go', async () => {
  const { input, screens, pointer, of } = setup();
  const touch = { pointerType: 'touch', pointerId: 1, clientX: 10, clientY: 10 };
  pointer(screens, 'pointerdown', touch);
  pointer(screens, 'pointermove', { ...touch, clientX: 60 }); // a drag: pressed at once
  assert.deepEqual(of(Input.DOWN), [0]);
  input.setTrackpad(true);
  assert.deepEqual(of(Input.UP), [0]);
});

test('switching out of trackpad mode mid-drag lets go, and the pointer stops looking pressed', async () => {
  const { input, screens, pointer, of } = setup();
  input.setTrackpad(true);
  const touch = { pointerType: 'touch', pointerId: 1 };
  pointer(screens, 'pointerdown', touch);
  await new Promise((r) => setTimeout(r, 520)); // held still: a drag
  assert.deepEqual(of(Input.DOWN), [0]);
  assert.ok(cursorEl.classes.has('down'));
  input.setTrackpad(false);
  assert.deepEqual(of(Input.UP), [0]);
  assert.ok(!cursorEl.classes.has('down'));
});

test('a long press is a right click, with nothing left to release after it', async () => {
  const { screens, pointer, of } = setup();
  const touch = { pointerType: 'touch', pointerId: 1 };
  pointer(screens, 'pointerdown', touch);
  await new Promise((r) => setTimeout(r, 520));
  assert.deepEqual(of(Input.DOWN), [0, 2]);
  assert.deepEqual(of(Input.UP), [0, 2]);
  pointer(screens, 'pointerup', touch);
  assert.deepEqual(of(Input.UP), [0, 2]);
});

test('Ctrl standing in for ⌘: it comes up with Ctrl, however Ctrl got there', () => {
  const { key, of } = setup();
  // Held since before this page had focus: the host never saw it go down.
  key('keydown', 'Tab', { ctrlKey: true });
  assert.deepEqual(of(Input.KEY_DOWN), [K.MetaLeft, K.Tab]);
  key('keyup', 'Tab', { ctrlKey: true });
  key('keyup', 'ControlLeft');
  assert.deepEqual(of(Input.KEY_UP), [K.Tab, K.MetaLeft]);
});

test('Ctrl standing in for ⌘: with both Ctrl keys, it stays until the last is up', () => {
  const { key, of } = setup();
  key('keydown', 'ControlLeft', { ctrlKey: true });
  key('keydown', 'ControlRight', { ctrlKey: true });
  key('keydown', 'Tab', { ctrlKey: true });
  key('keyup', 'Tab', { ctrlKey: true });
  key('keyup', 'ControlLeft', { ctrlKey: true });
  assert.ok(!of(Input.KEY_UP).includes(K.MetaLeft));
  key('keyup', 'ControlRight');
  assert.ok(of(Input.KEY_UP).includes(K.MetaLeft));
});

test('every keyboard setting ends a Ctrl stand-in, so it can start again', () => {
  for (const change of [(i) => i.setSwapCmdOption(false), (i) => i.setPcKeyboard(false), (i) => i.setCtrlAsCmd(true), (i) => i.setHostOs('macos')]) {
    const { input, key, of } = setup();
    key('keydown', 'ControlLeft', { ctrlKey: true });
    key('keydown', 'Tab', { ctrlKey: true });
    change(input);
    assert.ok(of(Input.KEY_UP).includes(K.MetaLeft));
    key('keydown', 'Space', { ctrlKey: true });
    assert.equal(of(Input.KEY_DOWN).filter((k) => k === K.MetaLeft).length, 2);
  }
});

const paste = (text) => doc.dispatchEvent(Object.assign(new Event('paste', { cancelable: true }), { clipboardData: { getData: (t) => (t === 'text/plain' ? text : ''), files: [] } }));
const pause = (ms) => new Promise((r) => setTimeout(r, ms));

test('⌘V: a paste event in time is the paste, with no keystroke', async () => {
  const { key, sent, of } = setup();
  key('keydown', 'KeyV', { metaKey: true });
  paste('hi');
  await pause(170);
  assert.deepEqual(sent.filter(([k]) => k === 'paste'), [['paste', 'hi']]);
  assert.ok(!of(Input.KEY_DOWN).includes(K.KeyV));
});

test('⌘V: a paste event after the keystroke went doesn\'t paste a second time', async () => {
  const { key, sent, of } = setup();
  key('keydown', 'KeyV', { metaKey: true });
  await pause(170);
  assert.ok(of(Input.KEY_DOWN).includes(K.KeyV));
  paste('late');
  assert.deepEqual(sent.filter(([k]) => k === 'paste'), []);
  // The next one is its own paste.
  key('keydown', 'KeyV', { metaKey: true });
  paste('next');
  assert.deepEqual(sent.filter(([k]) => k === 'paste'), [['paste', 'next']]);
  await pause(170);
});


test('viewer typing is gated to a capable Windows host and a Mac viewer', () => {
  for (const [os, macViewer, capability, text] of [
    ['windows', true, true, true], ['macos', true, true, false],
    ['linux', true, true, false], ['windows', false, true, false], ['windows', true, false, false],
  ]) {
    const events = [];
    const { input, key } = setup({ macViewer, send: (kind, fields) => events.push(kind === Input.TEXT
      ? new TextDecoder().decode(fields.bytes.slice(2, 2 + (fields.bytes[1] & 127))) : [kind, fields.keycode]) });
    input.setHostOs(os);
    input.setViewerTyping(capability);
    key('keydown', 'KeyY', { key: 'z' });
    key('keyup', 'KeyY', { key: 'z' });
    assert.deepEqual(events, text ? ['z'] : [[Input.KEY_DOWN, 16], [Input.KEY_UP, 16]]);
  }
});
