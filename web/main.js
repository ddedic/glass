// Main thread: wires input, UI and the worker together. Video and networking live
// in worker.js so the main thread only handles input and small DOM updates.

import { Input, inputEvent } from './proto.js';
import { attachInput } from './input.js';
import { attachGamepads } from './gamepad.js';
import { createUI, positionNames } from './ui.js';
import { log, setLogSink } from './log.js';
import { MAC_KEYCODES as K } from './keymap.js';
import { hostIdFor, serveRtc } from './rtc.js';
import { createChat } from './chat.js';
import { loadProfile, saveProfile } from './keyboard-profile.js';
import { answerPasskey } from './passkey.js';
import { createTerminal } from './terminal.js';
import { createAnnotator } from './annotate.js';
import { createThumbs } from './thumbs.js';
import { createAgent } from './agent.js';
import { createPush } from './push.js';
import { createDevice } from './device.js';
import { createFiles } from './files.js';
import { createCursor } from './cursor.js';

// Shortcuts iPadOS keeps for itself (no web page or app can capture them), sent as one tap.
const CMD = K.MetaLeft;
const SHORTCUTS = [
  { label: 'App switcher', keysLabel: '⌘⇥', keys: [CMD, K.Tab] },
  { label: 'Spotlight', keysLabel: '⌘Space', keys: [CMD, K.Space] },
  { label: 'Mission Control', keysLabel: '⌃↑', keys: [K.ControlLeft, K.ArrowUp] },
  { label: 'Hide app', keysLabel: '⌘H', keys: [CMD, K.KeyH] },
  { label: 'Quit app', keysLabel: '⌘Q', keys: [CMD, K.KeyQ] },
  { label: 'Close window', keysLabel: '⌘W', keys: [CMD, K.KeyW] },
  { label: 'Screenshot area', keysLabel: '⌘⇧4', keys: [CMD, K.ShiftLeft, K.Digit4] },
  { label: 'Force quit…', keysLabel: '⌘⌥⎋', keys: [CMD, K.AltLeft, K.Escape] },
  { label: 'Lock screen', keysLabel: '⌃⌘Q', keys: [K.ControlLeft, CMD, K.KeyQ] },
];

const TOKEN_KEY = 'glass.token';
const RESOLUTION_KEY = 'glass.resolution'; // 'auto' (sized to this screen) or 'native'
const SHARP_KEY = 'glass.sharp'; // '1': sharp text (for reading)
const PASS_KEY = 'glass.pass'; // guest pass from a magic link: { pass, expires }
const SWAP_KEY = 'glass.swapCmdOption';
const CTRL_AS_CMD_KEY = 'glass.ctrlAsCmd';
const DECODER_STEP_KEY = 'glass.decoderStep';
const PC_KEYBOARD_KEY = 'glass.pcKeyboard';
const MACHINE_KEY = 'glass.machine';
const REMOTES_KEY = 'glass.remotes'; // computers opened from anywhere, for glassdesk.dev/computers
// Give up on a remembered machine after this many failed connects and return to the
// machine that served this page.
const MACHINE_MAX_FAILURES = 3;
// ...or after this long without connecting: an unreachable host may never refuse, so
// attempts just time out slowly.
const MACHINE_CONNECT_TIMEOUT_MS = 12_000;
// Default to PC keyboard conventions on Linux, Windows and ChromeOS clients.
const isPC = /Linux|Windows|CrOS/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent);
const pcKeyboard = (storage((s) => s.getItem(PC_KEYBOARD_KEY)) ?? (isPC ? '1' : '0')) === '1';
// Phones and iPads freeze hidden pages and have few hardware decoders, so a hidden tab
// gives its decoders up at once (iPadOS reports itself as a Mac with a touchscreen).
const mobile = /iPhone|iPad|Android/.test(navigator.userAgent) || (/Macintosh/.test(navigator.userAgent) && navigator.maxTouchPoints > 1);
// Desktop browsers also call a window hidden whenever another one covers it (every Alt+Tab
// to a maximized app on Windows). Keep streaming that long, so switching back is instant
// instead of waiting for a keyframe and a new decoder.
const PAUSE_HIDDEN_AFTER_MS = 60_000;
const ctrlAsCmd = storage((s) => s.getItem(CTRL_AS_CMD_KEY)) !== '0'; // on by default

const screens = document.getElementById('screens');
const hud = document.getElementById('hud');
const keySink = document.getElementById('keys');

function storage(fn) {
  try { return fn(localStorage); } catch { return undefined; }
}

// The host prints a link with `#t=<token>`; remember it and drop it from the URL.
// A magic link carries `#l=<code>` instead: single use, so it's never stored.
// Remote access (glassdesk.dev/app/#r=<host key>) keeps `r` in the URL: it names the
// computer, and isn't secret.
const hashParams = new URLSearchParams(location.hash.slice(1));
const hashToken = hashParams.get('t');
const link = hashParams.get('l') ?? undefined;
const remoteKey = hashParams.get('r') ?? undefined;
const remoteId = remoteKey && (await hostIdFor(remoteKey));
// glassdesk.dev/app/ without a computer: the list of your computers instead. Stops here.
if (!remoteKey && location.pathname.startsWith('/app/')) {
  location.replace('/computers');
  await new Promise(() => {});
}
// Every remote computer shares this page's origin, so credentials are kept per computer:
// one computer's token must never be offered to another.
const tokenKey = remoteId ? `${TOKEN_KEY}.${remoteId}` : TOKEN_KEY;
const passKey = remoteId ? `${PASS_KEY}.${remoteId}` : PASS_KEY;
if (hashToken) storage((s) => s.setItem(tokenKey, hashToken));
// Back from single sign-on: the host handed this device a pass (like a link would).
const ssoPass = hashParams.get('p');
if (ssoPass) storage((s) => s.setItem(passKey, JSON.stringify({ pass: ssoPass, expires: Number(hashParams.get('e')) })));
if (hashToken || link || ssoPass) history.replaceState(null, '', location.pathname + location.search + (remoteKey ? `#r=${remoteKey}` : ''));

// Directory (LDAP) sign-in on the token screen: the host checks the password and hands
// back a guest pass, kept like one from a link; then connect with it.
{
  const box = document.getElementById('ldap');
  const field = (name) => box.querySelector(`input[name="${name}"]`);
  const error = document.getElementById('ldap-error');
  const submit = document.getElementById('ldap-submit');
  submit.addEventListener('click', async () => {
    const user = field('ldap-user').value.trim();
    const password = field('ldap-password').value;
    if (!user || !password) return;
    submit.disabled = true;
    error.classList.add('hidden');
    try {
      const r = await fetch('sso/ldap', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ user, password }) });
      const body = await r.json().catch(() => ({}));
      if (!r.ok || !body.pass) throw new Error(body.error ?? 'Sign-in didn\'t work. Try again.');
      storage((s) => s.setItem(passKey, JSON.stringify({ pass: body.pass, expires: body.expires })));
      location.reload();
    } catch (e) {
      error.textContent = e.message;
      error.classList.remove('hidden');
      field('ldap-password').select();
    } finally {
      submit.disabled = false;
    }
  });
  // Enter in these fields signs in here, not with the (empty) token form.
  box.addEventListener('keydown', (e) => {
    if (e.key !== 'Enter') return;
    e.preventDefault();
    e.stopPropagation();
    submit.click();
  });
}
const token = hashToken ?? storage((s) => s.getItem(tokenKey));
// From the app's shortcuts, a share or opened files (see manifest.webmanifest), or "use all
// my screens" (one window per Mac display).
const query = new URLSearchParams(location.search);
const openParam = query.get('open');
const sharedParam = query.has('shared');
const displayParam = query.get('display');
// A machine to open: from a per-machine app shortcut (?machine=dns) or a web+glass:// link.
const machineParam = query.get('machine') ?? (() => {
  const l = query.get('link');
  return l ? decodeURIComponent(l).replace(/^web\+glass:(\/\/)?/i, '').replace(/[/?#].*$/, '') || undefined : undefined;
})();
if (openParam || sharedParam || machineParam) history.replaceState(null, '', location.pathname + location.hash);
const savedPass = (() => {
  try {
    const p = JSON.parse(storage((s) => s.getItem(passKey)) ?? 'null');
    return p && p.expires * 1000 > Date.now() ? p.pass : undefined;
  } catch { return undefined; }
})();
let guest = false; // signed in with a magic link: no switching machines, no updating the host
// 'auto', 'native', or 'fit:WxH' ("fit this device": lasts only for this tab, so a later
// visit never reshapes the Mac's displays by itself).
let resolutionMode = (() => {
  try { const fit = sessionStorage.getItem('glass.fit'); if (fit) return fit; } catch {}
  return storage((s) => s.getItem(RESOLUTION_KEY)) === 'native' ? 'native' : 'auto';
})();
// Remote video over WebRTC (see videoFor / showStreams): declared early, set up below.
let rtc;
let rtpVideo = false;
let streamCount = 1;
const videos = [];
// Dynamic resolution: tell the host how many device pixels each display gets here (its
// area × pixel ratio × zoom), so it encodes no more and no less. "This Mac" asks for full
// resolution. Sizes come from ResizeObserver, so measuring never forces a layout.
let zoomScale = 1;
const areas = new Map(); // element -> [css width, css height]
const areaObserver = new ResizeObserver((entries) => {
  for (const e of entries) areas.set(e.target, [e.contentRect.width, e.contentRect.height]);
  queueViewport();
});
matchMedia(`(resolution: ${devicePixelRatio}dppx)`).addEventListener?.('change', () => queueViewport());
let viewportTimer;
let firstViewport = false; // this connection hasn't sent its viewport
let lastViewport = '';

// Remote: no host serves this page, so there's no config to fetch.
const config = remoteKey
  ? { wtPort: 0, wtHost: null, certHash: null, version: null }
  : await fetch('config.json').then((r) => r.json());

// Reload into new client code when the host's web files change (checked on reconnect
// and when the tab comes back), so a long-lived tab never runs stale code.
async function reloadIfUpdated(reason) {
  if (remoteKey) return;
  try {
    const latest = await fetch('config.json').then((r) => r.json());
    if (latest.version && latest.version !== config.version) {
      log('info', `client updated (${reason}), reloading`, { from: config.version, to: latest.version });
      setTimeout(() => location.reload(), 300); // let the log flush
    }
  } catch {}
}
const worker = new Worker('worker.js', { type: 'module' });
worker.onerror = (e) => log('error', `worker: ${e.message}`, { src: `${e.filename}:${e.lineno}` });

const control = (msg) => worker.postMessage({ type: 'control', msg });
// A view-only link sends no input (the host would drop it anyway).
let inputAllowed = true;
// Text events come ready made (textEvents); the rest from their fields.
const send = (kind, fields) => inputAllowed && worker.postMessage({ type: 'input', bytes: kind === Input.TEXT ? fields.bytes : inputEvent(kind, fields) });
let session = 0; // this session's id, from welcome (chat from it is our own)
let perms = { input: true, clipboard: true, files: true, owner: true };
let features = {};
let sharpWanted = storage((s) => s.getItem(SHARP_KEY)) === '1';
let sharpPending = false; // to ask for once the first picture is up
let sharpMode = 'off'; // what the host is doing: 'off', 'sharp' or '4:2:2'
let deniedMsg; // why the host last turned us away

// Resolves with the next clipboard the Mac reports (used for Cmd+C / Cmd+X):
// { text, html?, image } where `image` says there's one to fetch (macImage).
let clipboardWaiters = [];
let lastMacClipboard;
function nextClipboard(timeoutMs) {
  return new Promise((resolve, reject) => {
    const waiter = { resolve, reject };
    clipboardWaiters.push(waiter);
    setTimeout(() => {
      if (clipboardWaiters.includes(waiter)) {
        clipboardWaiters = clipboardWaiters.filter((w) => w !== waiter);
        // Nothing new (e.g. copied the same text again): fall back to the last known value.
        if (lastMacClipboard !== undefined) resolve(lastMacClipboard);
        else reject(new Error('no clipboard update'));
      }
    }, timeoutMs);
  });
}

// The Mac clipboard's image, fetched when needed (it can be several MB).
const IMAGE_WAIT_MS = 8000;
let imageWaiters = [];
function macImage() {
  return new Promise((resolve, reject) => {
    imageWaiters.push(resolve);
    control({ t: 'clipboardImage' });
    setTimeout(() => reject(new Error('the image never came')), IMAGE_WAIT_MS);
  });
}

/**
 * After ⌘C copied rich text or an image on the Mac (the text is already on this device's
 * clipboard): put the whole thing there. Browsers that want a tap for that (Safari) get a
 * Copy button.
 */
async function copyRich(clip) {
  if (!clip.html && !clip.image) return;
  const items = { 'text/plain': new Blob([clip.text], { type: 'text/plain' }) };
  if (clip.html) items['text/html'] = new Blob([clip.html], { type: 'text/html' });
  if (clip.image) {
    try {
      items['image/png'] = await macImage();
    } catch (e) {
      log('warn', `clipboard image: ${e.message}`);
    }
  }
  const write = () => navigator.clipboard.write([new ClipboardItem(items)]);
  try {
    await write();
    log('info', 'rich clipboard copied', { types: Object.keys(items) });
  } catch {
    // Text is there already; only an image is worth asking for a tap.
    if (!items['image/png']) return;
    ui.toast({
      text: `Image copied on ${machineName(currentHost)}`,
      icon: 'file',
      timeout: 10_000,
      action: { label: 'Copy here', run: () => write().catch((e) => ui.toast({ text: `Couldn't copy the image: ${e.message ?? e}`, kind: 'error' })) },
    });
  }
}

/** A clipboard from this device to the Mac; with `paste`, the Mac pastes it too. */
const MAX_CLIPBOARD_IMAGE = 8 << 20; // the host's limit
function sendClip({ text = '', html, image }, paste = false) {
  if (image?.size > MAX_CLIPBOARD_IMAGE) {
    ui.toast({ text: 'That image is too big to paste on the Mac (over 8 MB). Send it as a file instead.', kind: 'error' });
    image = undefined;
    if (!text && !html) return;
  }
  if (image) worker.postMessage({ type: 'sendClipboard', clip: { text, html, image, paste } });
  else control({ t: 'clipboard', text, ...(html && { html }), paste });
}

const statusEl = document.getElementById('status');
// Friendly machine names from the last machines.json, by machine id.
let machineNames = {};
const brandNames = {}; // machine -> its `glass config name`
const STATS_KEY = 'glass.stats';
let statsVisible = storage((s) => s.getItem(STATS_KEY)) === '1';
hud.classList.toggle('hidden', !statsVisible);
let tokenTried = Boolean(token);
let restarting; // 'Glass' or 'computer' after a restart request, until it's back
// Above the input layer: in draw mode it takes the pointer, so nothing reaches the host.
const annotator = createAnnotator({ screens, send: control });
const input = attachInput({
  screens,
  keySink,
  send,
  control,
  nextClipboard,
  onMacCopy: (clip) => copyRich(clip),
  onPaste: (clip) => sendClip(clip, true),
  onLocalShortcut: (code) => (code === 'KeyF' ? toggleFullscreen() : toggleStats()),
  swapCmdOption: storage((s) => s.getItem(SWAP_KEY)) === '1',
  ctrlAsCmd,
  pcKeyboard,
  onZoom: (s) => {
    zoomScale = s;
    queueViewport();
    annotator.invalidate();
    document.getElementById('zoom-pill').classList.toggle('hidden', s === 1);
    document.getElementById('zoom-level').textContent = `${s.toFixed(1)}×`;
    cursor.refresh();
  },
  onResize: () => cursor.refresh(),
});
// The host's pointer, drawn here rather than in the video (where the host supports it).
const cursor = createCursor({
  screens,
  overlay: document.getElementById('remote-cursor'),
  contentRect: (el) => input.contentRect(el),
  onWanted: (on) => worker.postMessage({ type: 'localCursor', on }),
});
document.getElementById('zoom-reset').addEventListener('click', () => input.resetZoom());
let micContext;
let micStream;
const gamepads = attachGamepads({
  send,
  log,
  onConnect: (g) => ui.toast({ text: `Controller connected: ${g.id.replace(/\s*\(.*$/, '') || 'gamepad'}`, timeout: 3000 }),
});

// Drawing on the screen (everyone connected sees it), and recording on the host.
const drawButton = document.getElementById('btn-draw');
const recordButton = document.getElementById('btn-record');
let recording = false;
annotator.onChange((on) => drawButton.setAttribute('aria-pressed', String(on)));
drawButton.addEventListener('click', (e) => {
  e.preventDefault();
  annotator.toggle();
});
recordButton.addEventListener('click', (e) => {
  e.preventDefault();
  control({ t: 'record', on: !recording });
});
function onRecording({ on, path }) {
  recording = on;
  recordButton.setAttribute('aria-pressed', String(on));
  recordButton.title = on ? 'Stop recording' : 'Record on the host';
  const where = path?.replace(/^\/(Users|home)\/[^/]+/, '~');
  if (on) ui.toast({ text: 'Recording on the host', timeout: 3000 });
  else if (where) ui.toast({ text: `Recording saved on ${machineName(currentHost)}: ${where}`, icon: 'file', timeout: 8000 });
  log('info', `recording ${on ? 'started' : 'stopped'}`, { path });
}

const ui = createUI({
  onOpenMachines: loadMachines,
  onWakeMachine: (m) => control({ t: 'wake', dns: m.dns }),
  onSelectMachine: (m) => switchMachine(machineId(m), m.name),
  onSelectDisplay: (id) => control(id === 'all' ? { t: 'showAll' } : { t: 'selectDisplay', id }),
  onSharp: (on) => {
    sharpWanted = on;
    sharpPending = false;
    storage((s) => s.setItem(SHARP_KEY, on ? '1' : '0'));
    ui.setSharp({ on });
    control({ t: 'sharp', on });
    // Back to sizing by this screen: the host ignored our viewport while sharp.
    if (!on) queueViewport(true);
    log('info', `sharp text ${on ? 'on' : 'off'}`);
  },
  onListWindows: () => control({ t: 'listWindows' }),
  onSelectWindow: (id) => {
    streamedWindows.add(id);
    control({ t: 'selectWindow', id });
  },
  shortcuts: SHORTCUTS,
  swapCmdOption: storage((s) => s.getItem(SWAP_KEY)) === '1',
  ctrlAsCmd,
  pcKeyboard,
  onPcKeyboard: (on) => {
    storage((s) => s.setItem(PC_KEYBOARD_KEY, on ? '1' : '0'));
    keyboard.pc = on;
    saveKeyboard();
    input.setPcKeyboard(on);
    log('info', `pc keyboard ${on ? 'on' : 'off'}`);
  },
  onPhysicalKeyboard: (on) => {
    keyboard.physical = on;
    saveKeyboard();
    input.setPhysicalKeyboard(on);
  },
  onCtrlAsCmd: (on) => {
    storage((s) => s.setItem(CTRL_AS_CMD_KEY, on ? '1' : '0'));
    keyboard.ctrlAsCmd = on;
    saveKeyboard();
    input.setCtrlAsCmd(on);
    log('info', `ctrl as cmd ${on ? 'on' : 'off'}`);
  },
  onSwapCmdOption: (on) => {
    storage((s) => s.setItem(SWAP_KEY, on ? '1' : '0'));
    keyboard.swap = on;
    saveKeyboard();
    input.setSwapCmdOption(on);
    log('info', `swap cmd/option ${on ? 'on' : 'off'}`);
  },
  onShortcut: (sc) => input.sendCombo(sc.keys),
  resolution: resolutionMode,
  onResolution: (mode) => setResolution(mode),
  fitSizes: () => fitSizes(),
  deviceName: () => deviceLabel().replace(/^.* on /, ''),
  onKeyboard: () => input.focusKeys(),
  onUploadFiles: uploadFiles,
  onRequestFiles: () => {
    control({ t: 'pickFiles' });
    ui.toast({ text: 'Choose files in the dialog on the computer' });
  },
  onAudio: toggleAudio,
  onMic: toggleMic,
  onFullscreen: toggleFullscreen,
  onToggleStats: toggleStats,
  onPrivacy: (on) => control({ t: 'privacy', on }),
  onSas: () => control({ t: 'sas' }),
  onRestart: (glass) => {
    restarting = glass ? 'Glass' : 'computer';
    control({ t: 'restart', glass });
    log('info', `restart ${restarting}`);
    onConnectionStatus('restarting');
  },
  onTerminal: () => terminal.toggle(),
  onTrackpad: () => setTrackpad(!trackpadOn, true),
  onPad: (on) => setPad(on, true),
  onExtend: (on) => setExtended(on),
  onOpenDevice: () => {
    storage((s) => s.setItem('glass.deviceSeen', '1'));
    ui.setDeviceHint(false);
    device.render();
  },
  onToken: (t) => {
    signedInToken = t;
    tokenTried = true;
    showStatus('connecting', 'Connecting', machineName(currentHost));
    storage((s) => s.setItem(tokenKey, t));
    worker.postMessage({ type: 'token', token: t });
  },
});
const terminal = createTerminal({ control, log });
// Which display each stream shows, for placing the agent's marker.
let streamIds = [];
const agent = createAgent({
  control,
  locate(display, x, y) {
    const i = streamIds.indexOf(display);
    const el = (rtpVideo ? videos : canvases)[i];
    if (i < 0 || !el) return null;
    const [w, h] = (videoSizes[i] ?? '').split('×').map(Number);
    const r = el.getBoundingClientRect();
    if (!w || !h || !r.width) return null;
    // object-fit: contain centres the picture in its box.
    const scale = Math.min(r.width / w, r.height / h);
    const left = r.left + (r.width - w * scale) / 2;
    const top = r.top + (r.height - h * scale) / 2;
    return { x: left + x * w * scale, y: top + y * h * scale };
  },
});
const thumbs = createThumbs({
  control,
  onSelect: (id) => control(id === 'all' ? { t: 'showAll' } : { t: 'selectDisplay', id }),
  names: positionNames,
  screens,
  pictures: () =>
    (rtpVideo ? videos : canvases).slice(0, streamCount).map((el, i) => {
      const [w, h] = (videoSizes[i] ?? '').split('×').map(Number);
      return { el, w, h };
    }),
});
// The token this page signed in with (if any), for opening forwarded ports.
let signedInToken = token;

/** A forwarded port on this host: with a token, via /fwd/auth so the tab gets its cookie. */
function portUrl(port) {
  const path = `/fwd/${port}/`;
  return signedInToken ? `${location.origin}/fwd/auth?t=${encodeURIComponent(signedInToken)}&to=${encodeURIComponent(path)}` : `${location.origin}${path}`;
}

// This machine's keyboard remaps (each machine keeps its own).
let keyboard = { pc: pcKeyboard, ctrlAsCmd, swap: storage((s) => s.getItem(SWAP_KEY)) === '1', physical: false };
// Keyed by host and port, so two Glass hosts on one name (tests, or several on a box) differ.
const profileId = () => (currentHost.includes(':') ? currentHost : `${currentHost}:${config.wtPort}`);
function saveKeyboard() {
  storage((s) => saveProfile((k, v) => s.setItem(k, v), profileId(), keyboard));
}
function applyKeyboard(hostOs) {
  const p = storage((s) => loadProfile((k) => s.getItem(k), profileId(), { fallback: keyboard, hostOs, isPC })) ?? keyboard;
  keyboard = { pc: p.pc, ctrlAsCmd: p.ctrlAsCmd, swap: p.swap, physical: p.physical };
  input.setPcKeyboard(keyboard.pc);
  input.setCtrlAsCmd(keyboard.ctrlAsCmd);
  input.setSwapCmdOption(keyboard.swap);
  input.setPhysicalKeyboard(keyboard.physical);
  ui.setKeyboardProfile(keyboard);
}

const chat = createChat({ onSend: (text) => control({ t: 'chat', text }), toast: (o) => ui.toast(o) });
chat.setAvailable(false);

const files = createFiles({
  control,
  log,
  toast: (o) => ui.toast(o),
  upload: (list, dir) => uploadFiles(list, dir),
  // A plain browser download, from this page's own host (not over remote access).
  // `name` '': the host's name for it (Content-Disposition).
  download: (url, name) => {
    const a = document.createElement('a');
    a.href = url;
    a.download = name;
    a.click();
  },
  rawUrl: (ticket, path) =>
    ticket && !remoteKey && currentHost === homeHost
      ? `${location.origin}/files/raw?t=${encodeURIComponent(ticket)}&path=${encodeURIComponent(path)}`
      : undefined,
});

const push = createPush({
  control,
  log,
  toast: (o) => ui.toast(o),
  // Where a notification opens: this page (remote: with the computer's key, not secret).
  url: `${location.origin}${location.pathname}${remoteKey ? `#r=${remoteKey}` : ''}`,
  actions: !remoteKey,
});
// A struggling device asks for a smaller picture (Compute Pressure).
let pressureScale = 1;

// Launched from the home screen there's no browser UI to leave, so fullscreen is moot.
const standalone = matchMedia('(display-mode: standalone), (display-mode: fullscreen)').matches || navigator.standalone;
if (standalone) document.getElementById('btn-fullscreen').classList.add('hidden');
log('info', 'display mode', { standalone: !!standalone });

const device = createDevice({
  ui,
  push,
  control,
  log,
  input,
  screens,
  streams: () => (rtpVideo ? videos : canvases).slice(0, streamCount),
  sendClip: (clip) => sendClip(clip),
  displays: () => displays,
  onPause: (away) => worker.postMessage({ type: 'visible', visible: !away && !padOn && document.visibilityState === 'visible' }),
  onPressure: (scale) => {
    if (scale === pressureScale) return;
    pressureScale = scale;
    log('info', `cpu pressure: picture at ${scale}×`);
    queueViewport(true);
  },
  standalone,
});

// No token is fine on your own tailnet; the host asks for one only if it can't identify us.

// The page can drive any Glass host on the tailnet: same client, different WebTransport
// endpoint. Start on the machine picked last time, if any.
const homeHost = config.wtHost ?? location.hostname;
// A shortcut or link naming a machine: its full name (or name:port), or a short one
// ("studio") looked up among this tailnet's machines.
if (machineParam && !remoteKey) {
  let id = machineParam;
  if (!id.includes('.') && id !== 'localhost') {
    const { machines = [] } = await fetch('machines.json').then((r) => r.json()).catch(() => ({}));
    const want = id.toLowerCase();
    const m = machines.find((x) => x.glass && (x.name.toLowerCase() === want || x.dns.split('.')[0].toLowerCase() === want));
    id = m ? (m.wt_port && m.wt_port !== config.wtPort ? `${m.dns}:${m.wt_port}` : m.dns) : homeHost;
  }
  storage((s) => (id === homeHost ? s.removeItem(MACHINE_KEY) : s.setItem(MACHINE_KEY, id)));
}
let currentHost = storage((s) => s.getItem(MACHINE_KEY)) ?? homeHost;
let machineFailures = 0;
let machineTimer;

function giveUpOnMachine() {
  ui.toast({ text: `Couldn't reach ${currentHost}; back to this machine`, kind: 'error' });
  switchMachine(homeHost);
}

/** Falls back to the home machine if `currentHost` doesn't connect in time. */
function armMachineTimeout() {
  clearTimeout(machineTimer);
  if (currentHost !== homeHost) machineTimer = setTimeout(giveUpOnMachine, MACHINE_CONNECT_TIMEOUT_MS);
}
// A machine is "host" (default port) or "host:port".
const wtUrl = (machine) => `https://${machine.includes(':') ? machine : `${machine}:${config.wtPort}`}/`;
const machineId = (m) => (m.wt_port && m.wt_port !== config.wtPort ? `${m.dns}:${m.wt_port}` : m.dns);

// Remote access: the WebRTC connection lives here (the main thread) and is relayed to the
// worker over a MessagePort.
let rtcPort;
if (remoteKey) {
  const channel = new MessageChannel();
  const signalUrl = `${location.origin.replace(/^http/, 'ws')}/signal/connect/${remoteId}`;
  rtc = serveRtc({
    port: channel.port1,
    hostKey: remoteKey,
    signalUrl,
    log,
    onVideo: (stream, track) => {
      const v = videoFor(stream);
      v.srcObject = new MediaStream([track]);
      v.play().catch(() => {});
    },
    onMode: (rtp) => {
      rtpVideo = rtp;
      codec = rtp ? 'H.264 · WebRTC' : codec;
      log('info', `remote video: ${rtp ? 'RTP tracks' : 'data channel'}`);
      showStreams(streamCount);
    },
  });
  rtcPort = channel.port2;
}

worker.postMessage({
  type: 'init',
  url: wtUrl(currentHost),
  certHash: config.certHash,
  token,
  link,
  pass: savedPass,
  rtcPort,
  device: deviceLabel(),
  // A wide-colour screen: a Display P3 stream is drawn in P3, not clipped to sRGB.
  wideColor: matchMedia('(color-gamut: p3)').matches,
  decoderStep: Number(storage((s) => s.getItem(DECODER_STEP_KEY)) ?? 0),
  // bench/ only: read the test host's frame marker (see bench.js).
  bench: storage((s) => s.getItem('glass.bench')) === '1',
}, rtcPort ? [rtcPort] : []);
armMachineTimeout();
showStatus('connecting', 'Connecting', machineName(currentHost));
ui.setStatsPressed(statsVisible);

// Canvases are created on demand (one per host stream) and handed to the worker once;
// extras are hidden rather than destroyed so they can be reused.
const canvases = [];
// Remote viewers get video as WebRTC tracks, shown in <video> elements in place of the
// worker's canvases (input maps onto either: both carry data-stream).

function videoFor(stream) {
  while (videos.length <= stream) {
    const i = videos.length;
    const v = document.createElement('video');
    v.dataset.stream = String(i);
    v.tabIndex = -1;
    v.autoplay = true;
    v.muted = true;
    v.playsInline = true;
    v.disablePictureInPicture = true;
    v.disableRemotePlayback = true;
    v.setAttribute('playsinline', '');
    v.style.order = String(i);
    v.classList.add('hidden');
    v.addEventListener('resize', () => {
      if (!v.videoWidth) return;
      videoSizes[i] = `${v.videoWidth}×${v.videoHeight}`;
      input.setVideoSize(i, v.videoWidth, v.videoHeight);
      thumbs.measure();
      hideStatus();
    });
    screens.append(v);
    input.observeCanvas(v);
    areaObserver.observe(v);
    videos.push(v);
  }
  return videos[stream];
}

function showStreams(count) {
  streamCount = count;
  if (rtpVideo) {
    for (let i = 0; i < count; i++) videoFor(i);
    videos.forEach((v, i) => v.classList.toggle('hidden', i >= count));
    canvases.forEach((c) => c.classList.add('hidden'));
    return;
  }
  videos.forEach((v) => v.classList.add('hidden'));
  while (canvases.length < count) {
    const canvas = document.createElement('canvas');
    canvas.dataset.stream = String(canvases.length);
    canvas.style.order = String(canvases.length);
    canvas.tabIndex = -1;
    screens.append(canvas);
    input.observeCanvas(canvas);
    areaObserver.observe(canvas);
    const offscreen = canvas.transferControlToOffscreen();
    worker.postMessage({ type: 'canvas', stream: canvases.length, canvas: offscreen }, [offscreen]);
    canvases.push(canvas);
  }
  canvases.forEach((c, i) => c.classList.toggle('hidden', i >= count));
  worker.postMessage({ type: 'streams', count });
}
showStreams(1);
if (document.visibilityState !== 'visible') worker.postMessage({ type: 'visible', visible: false });

const videoSizes = [];
let status = 'connecting…';
let codec = '';
let statsCount = 0;
let loggedPeak = 0; // the highest audioPeak since stats were last logged
let targetMbps;
let displays = [];
// Ids of windows (not displays) this viewer has seen or picked, so a streamed window never
// shows up in the displays list.
const streamedWindows = new Set();
const transfers = new Map(); // id -> toast

worker.onmessage = ({ data }) => {
  switch (data.type) {
    case 'cursor':
      cursor.setPosition(data.pos);
      break;
    case 'connected':
      reloadIfUpdated('reconnect');
      if (rtpVideo) checkRtpVideoArrives();
      break;
    case 'status':
      if (data.level === 'error' && /connection failed/.test(data.text) && currentHost !== homeHost) {
        if (++machineFailures >= MACHINE_MAX_FAILURES) giveUpOnMachine();
      }
      status = data.text;
      // Logs wait (in log.js) until the next session is signed in.
      if (status !== 'connected') setLogSink(undefined);
      onConnectionStatus(status);
      log(data.level ?? 'info', `status: ${status}`);
      break;
    case 'log':
      log(data.level, data.msg, data.data);
      break;
    case 'codec':
      codec = data.codec;
      log('info', `decoder configured: ${codec}`);
      break;
    case 'size':
      videoSizes[data.stream] = `${data.width}×${data.height}`;
      input.setVideoSize(data.stream, data.width, data.height);
      thumbs.measure();
      annotator.invalidate();
      hideStatus();
      break;
    case 'stats':
      // Over RTP the worker sees no video; the connection's own stats describe it.
      if (rtpVideo) data = { ...data, ...rtpStats };
      // Logged every fifth: with the loudest sound since the last one logged.
      loggedPeak = Math.max(loggedPeak, data.audioPeak ?? 0);
      if (++statsCount % 5 === 0) {
        log('info', 'stats', { ...data, audioPeak: loggedPeak });
        loggedPeak = 0;
      }
      if (data.fps > 0) hideStatus();
      renderHud(data);
      break;
    case 'firstFrame':
      window.glassFirstFrame ??= performance.now();
      // Sharp text restarts capture and the encoder: not while the first picture is coming.
      if (sharpPending) control({ t: 'sharp', on: true });
      sharpPending = false;
      startAfterFirstPicture(0);
      break;
    case 'bench':
      // One second of the bench probe, for bench/run.mjs to collect.
      (window.glassBench ??= []).push(data);
      break;
    case 'decoderStep':
      storage((s) => s.setItem(DECODER_STEP_KEY, String(data.step)));
      break;
    case 'unauthorized':
      storage((s) => s.removeItem(tokenKey));
      storage((s) => s.removeItem(passKey));
      log('warn', 'token rejected');
      hideStatus();
      ui.showAuth({ rejected: tokenTried });
      // Single sign-on is this host's own web page, so not from a remote (WebRTC) viewer.
      document.getElementById('sso').classList.toggle('hidden', !(config.sso && !remoteKey));
      document.getElementById('ldap').classList.toggle('hidden', !(config.ldap && !remoteKey));
      break;
    case 'control':
      onControl(data.msg);
      break;
    case 'fileProgress':
      onFileProgress(data);
      break;
    case 'fileReceived':
      onFileReceived(data);
      break;
    case 'clipboardImage':
      for (const w of imageWaiters) w(data.image);
      imageWaiters = [];
      break;
    case 'audio':
      ui.setAudioOn(data.on);
      if (!data.on) audioContext?.suspend();
      log('info', `audio ${data.on ? 'on' : 'off'}`);
      break;
  }
};

// On a slow link the first picture has to share it with everything else, so it goes first:
// the main screen, then sound and the mic (the worker), then the other displays' previews,
// then the rest. Only a picture that never comes (a hidden page) holds them to the wait.
const FIRST_PICTURE_WAIT_MS = 6000;
const PREVIEWS_AFTER_MS = 1000;
const REST_AFTER_MS = 2000;
let startTimers = [];
function cancelStart() {
  startTimers.forEach(clearTimeout);
  startTimers = [];
}
function startAfterFirstPicture(wait) {
  cancelStart();
  startTimers = [
    setTimeout(() => thumbs.setConnected(true), wait + PREVIEWS_AFTER_MS),
    setTimeout(() => { if (remoteKey) setLogSink((lines) => control({ t: 'log', lines })); }, wait + REST_AFTER_MS),
  ];
}

function onControl(msg) {
  switch (msg.t) {
    case 'welcome':
      startAfterFirstPicture(FIRST_PICTURE_WAIT_MS);
      cursor.reset();
      cursor.setHostSupports(Boolean(msg.features?.cursor));
      agent.reset();
      agent.setOwner(Boolean(msg.perms ? msg.perms.owner : !msg.guest));
      machineFailures = 0;
      clearTimeout(machineTimer);
      displays = msg.displays;
      applyBranding(msg);
      // Quick support (`glass support`): this viewer is someone helping.
      if (msg.features?.support) {
        const who = msg.name ?? 'them';
        document.title = `Helping ${who}`;
        ui.toast({ text: `You're helping ${who}. They can end this any time.`, timeout: 6000 });
      }
      // Nothing being streamed (e.g. capture failed): fall back to the first display.
      if (!msg.streams.length && displays.length) control({ t: 'selectDisplay', id: displays[0].id });
      ui.setAudioAvailable(msg.audio);
      input.setHostOs(msg.host_os ?? 'macos');
      ui.setHostOs(msg.host_os ?? 'macos');
      applyKeyboard(msg.host_os ?? 'macos');
      files.setHostOs(msg.host_os ?? 'macos');
      if (remoteKey) rememberRemote(msg.computer);
      guest = Boolean(msg.guest);
      // Remote: other machines aren't reachable from here, so no machines menu either.
      ui.setGuest(guest || Boolean(remoteKey));
      ui.setCanFit(!guest && msg.features?.fit !== false);
      // A new session has no second display (the host removed it when the last one ended).
      extended = undefined;
      ui.setExtend({ available: Boolean(msg.features?.extend && (msg.perms ? msg.perms.owner : !guest)), on: false });
      session = msg.session ?? 0;
      perms = msg.perms ?? perms;
      features = msg.features ?? {};
      inputAllowed = perms.input;
      ui.setPerms(perms);
      chat.setAvailable(true);
      files.setAvailable(Boolean(msg.features?.browse));
      restarting = undefined;
      onNowPlaying(null); // the host says what it's playing right after welcome
      push.connected({ key: msg.features?.push, isOwner: Boolean(msg.perms ? msg.perms.owner : !guest) }).then(() => {
        // Point out the new menu until it's been opened once (notifications are off).
        if (push.state === 'off' && storage((s) => s.getItem('glass.deviceSeen')) !== '1') ui.setDeviceHint(true);
      });
      device.setConnected(true);
      onFirstWelcome();
      ui.setComputerControls({
        isOwner: msg.perms ? msg.perms.owner : !guest,
        privacy: Boolean(msg.features?.privacy),
        // The console is served by the host this page came from (not over remote access).
        console: !remoteKey && currentHost === homeHost
          ? (signedInToken ? `${location.origin}/admin/auth?t=${encodeURIComponent(signedInToken)}` : `${location.origin}/admin/`)
          : undefined,
      });
      ui.setPrivacyOn(false);
      ui.setWindowsAvailable(Boolean(msg.features?.windows));
      // Sharp text is this device's choice: ask again on every (re)connect.
      sharpMode = 'off';
      ui.setSharp({ available: perms.input, on: sharpWanted && perms.input, mode: 'off' });
      sharpPending = sharpWanted && perms.input;
      {
        // Controllers and the mic count as input: not for view-only links.
        const canInput = msg.perms?.input ?? true;
        gamepads.setEnabled(Boolean(msg.features?.gamepad) && canInput);
        gamepads.resend();
        // On a Mac without its driver yet, the owner still gets the button: it offers to
        // install it.
        micReady = Boolean(msg.features?.mic);
        micInstallable = Boolean(msg.features?.mic_install);
        ui.setMicAvailable((micReady || micInstallable) && canInput);
        if (!canInput && micStream) stopMic();
      }
      terminal.reset();
      terminal.setAvailable(Boolean(msg.perms?.owner && msg.features?.terminal));
      // Forwarded ports are this page's host's, over plain HTTPS on the tailnet.
      ui.setPorts(msg.perms?.owner && !remoteKey && currentHost === homeHost ? msg.features?.forward_ports ?? [] : [], portUrl);
      annotator.setSession(msg.session);
      recordButton.classList.toggle('hidden', !(msg.features?.record && msg.perms?.owner));
      if (guest && resolutionMode.startsWith('fit:')) setResolution('auto');
      onLayout(msg.streams);
      // At once: the host holds the first picture for it, to send it at this size.
      firstViewport = true;
      lastViewport = '';
      sendViewport();
      // A reload or reconnect: the host put the displays back when the session ended.
      if (resolutionMode.startsWith('fit:') && !guest) sendFit();
      log('info', 'welcome', { displays: displays.length, streams: msg.streams.map((d) => d.id), audio: msg.audio });
      break;
    case 'cursor':
      cursor.setShape(msg);
      break;
    case 'layout':
      // The host's current displays (a fit adds a virtual one and hides the one it mirrors).
      if (msg.displays) displays = msg.displays;
      onLayout(msg.streams);
      break;
    case 'waiting':
      showStatus('connecting', 'Waiting to be let in', msg.msg);
      break;
    case 'denied':
      deniedMsg = msg.msg;
      showStatus('error', 'Not let in', msg.msg);
      ui.toast({ text: msg.msg, kind: 'error', timeout: 0, action: { label: 'Ask again', run: () => {
        showStatus('connecting', 'Connecting', machineName(currentHost));
        worker.postMessage({ type: 'askAgain' });
      } } });
      break;
    case 'nowPlaying':
      onNowPlaying(msg.info);
      break;
    case 'micReady':
      micReady = true;
      ui.toast({ text: `The microphone works on ${machineName(currentHost)} now.`, timeout: 6000, action: { label: 'Turn on', run: () => toggleMic() } });
      break;
    case 'notice':
      ui.toast({ text: msg.msg });
      break;
    case 'push':
      push.onMessage(msg);
      break;
    case 'files':
    case 'filesThumb':
      files.onMessage(msg);
      break;
    case 'chat':
      if (msg.sender !== session) chat.receive(msg);
      break;
    case 'thumbs':
      thumbs.onMessage(msg);
      break;
    case 'agentStatus':
    case 'agentAction':
    case 'agentAsk':
    case 'agentAnswered':
      agent.onMessage(msg);
      break;
    case 'windows':
      for (const w of msg.windows) streamedWindows.add(w.id);
      ui.renderWindows(msg.windows);
      break;
    case 'pass':
      storage((s) => s.setItem(passKey, JSON.stringify({ pass: msg.pass, expires: msg.expires })));
      ui.toast({ text: `You're in. This device has access until ${formatExpiry(msg.expires)}.`, icon: 'link', timeout: 6000 });
      log('info', 'signed in with a magic link', { expires: msg.expires });
      break;
    case 'sharp':
      sharpMode = msg.mode;
      ui.setSharp({ mode: msg.mode });
      log('info', `sharp text: ${msg.mode}`);
      break;
    case 'privacy':
      ui.setPrivacyOn(msg.on);
      log('info', `privacy screen ${msg.on ? 'on' : 'off'}`);
      break;
    case 'bitrate':
      targetMbps = msg.bps / 1e6;
      break;
    case 'updateAvailable':
      offerUpdate(msg);
      break;
    case 'clipboard':
      log('info', 'mac clipboard changed', { length: msg.text.length, html: Boolean(msg.html), image: Boolean(msg.image) });
      lastMacClipboard = { text: msg.text, html: msg.html, image: Boolean(msg.image) };
      for (const w of clipboardWaiters) w.resolve(lastMacClipboard);
      clipboardWaiters = [];
      break;
    case 'term':
      terminal.onMessage(msg);
      break;
    case 'fileSaved':
      files.onFileSaved();
      ui.toast({ text: `Saved on ${machineName(currentHost)}: ${msg.path.replace(/^\/(Users|home)\/[^/]+/, '~')}`, icon: 'file' });
      break;
    case 'passkey':
      // A --passkey link: create one now, or prove this is the device that did.
      showStatus('connecting', 'Confirm with your passkey…', msg.mode === 'create' ? 'This link asks this device to save a passkey for Glass.' : machineName(currentHost));
      answerPasskey(msg).then((answer) => {
        if (answer.mode === 'error') log('warn', `passkey: ${answer.error}`);
        else log('info', `passkey ${answer.mode} answered`);
        control({ t: 'passkey', ...answer });
      });
      break;
    case 'annotate':
    case 'annotateClear':
      annotator.onMessage(msg);
      break;
    case 'recording':
      onRecording(msg);
      break;
    case 'error':
      if (msg.msg !== 'unauthorized') ui.toast({ text: msg.msg, kind: 'error', timeout: 6000 });
      log('error', `host: ${msg.msg}`);
      break;
  }
}

// ---- Connection state overlay and stats HUD ----


/** "Safari on iPad", "Chrome on Windows": how the host lists this viewer. */
function deviceLabel() {
  const ua = navigator.userAgent;
  const browser = /Edg\//.test(ua) ? 'Edge' : /Firefox\//.test(ua) ? 'Firefox'
    : /Chrome\//.test(ua) ? 'Chrome' : /Safari\//.test(ua) ? 'Safari' : 'A browser';
  // iPadOS reports itself as a Mac; touch gives it away.
  const os = /iPhone/.test(ua) ? 'iPhone'
    : /iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1) ? 'iPad'
    : /Android/.test(ua) ? 'Android' : /CrOS/.test(ua) ? 'ChromeOS' : /Windows/.test(ua) ? 'Windows'
    : /Macintosh/.test(ua) ? 'Mac' : /Linux/.test(ua) ? 'Linux' : '';
  return os ? `${browser} on ${os}` : browser;
}

function machineName(id) {
  if (brandNames[id]) return brandNames[id];
  if (remoteKey) return 'your computer';
  return machineNames[id] ?? id.split('.')[0].split(':')[0];
}

/** Lists this computer at glassdesk.dev/computers (same origin), newest first. */
function rememberRemote(name) {
  storage((s) => {
    const others = JSON.parse(s.getItem(REMOTES_KEY) ?? '[]').filter((c) => c.key !== remoteKey);
    s.setItem(REMOTES_KEY, JSON.stringify([{ key: remoteKey, name: name || 'Computer' }, ...others].slice(0, 32)));
  });
}

// Branding (glass config name / accent), from a machine's welcome.
function applyBranding({ name, accent }) {
  if (name) brandNames[currentHost] = name;
  else delete brandNames[currentHost];
  document.title = name ?? 'Glass';
  const root = document.documentElement.style;
  if (/^#[0-9a-f]{6}$/i.test(accent ?? '')) {
    const [r, g, b] = [1, 3, 5].map((i) => parseInt(accent.slice(i, i + 2), 16) / 255);
    // WCAG relative luminance: dark ink on light accents, light ink on dark ones.
    const lin = (c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
    const luminance = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
    root.setProperty('--accent', accent);
    root.setProperty('--accent-ink', luminance > 0.18 ? '#04121f' : '#ffffff');
  } else {
    root.removeProperty('--accent');
    root.removeProperty('--accent-ink');
  }
}

function showStatus(state, title, detail = '') {
  statusEl.dataset.state = state;
  document.getElementById('status-title').textContent = title;
  document.getElementById('status-detail').textContent = detail;
  statusEl.classList.remove('gone');
}

function hideStatus() {
  statusEl.classList.add('gone');
}

/** Maps the worker's status text to what the person should see. */
// [title, detail] while another tab of this browser has the session (see navigator.locks).
let elsewhere;

function onConnectionStatus(text) {
  if (elsewhere) {
    cancelStart();
    thumbs.setConnected(false);
    device.setConnected(false);
    showStatus('error', ...elsewhere);
    return;
  }
  if (text !== 'connected') {
    cancelStart();
    thumbs.setConnected(false);
    device.setConnected(false);
  }
  const name = machineName(currentHost);
  // Restarting: the connection drops and retries for a while; that's expected.
  if (restarting && text !== 'connected' && !text.startsWith('waiting for access token')) {
    showStatus('reconnecting', 'Restarting…', restarting === 'Glass' ? `Glass is restarting on ${name}. Reconnecting…` : `${name} is restarting. This reconnects once it's back and someone is logged in.`);
    return;
  }
  if (text === 'connected') showStatus('connecting', `Connected to ${name}`, 'Waiting for the screen…');
  else if (text.startsWith('disconnected')) showStatus('reconnecting', 'Reconnecting', `Lost the connection to ${name}. Trying again…`);
  else if (text.startsWith('connection failed') && remoteKey) {
    // Remote: the reason is specific (offline, identity check, no route), so show it.
    const reason = text.replace(/^connection failed: /, '').replace(/, retrying…$/, '');
    showStatus('error', `Can't reach ${name}`, `${reason} Retrying…`);
  } else if (text.startsWith('connection failed'))
    showStatus('error', `Can't reach ${name}`, 'Retrying. Make sure Glass is running there and both devices are on Tailscale.');
  else if (text.startsWith('waiting for access token')) hideStatus();
  else if (text === 'not let in') showStatus('error', 'Not let in', deniedMsg ?? `The person at ${name} didn't let you in.`);
}

function hudLine(...parts) {
  const div = document.createElement('div');
  div.append(...parts);
  return div;
}

function latencyClass(ms) {
  return ms < 40 ? 'good' : ms < 80 ? 'warn' : 'bad';
}

function renderHud(d) {
  const host = document.createElement('b');
  host.textContent = machineName(currentHost);
  const latency = document.createElement('span');
  latency.className = latencyClass(d.latencyAvg);
  latency.textContent = `${d.latencyAvg.toFixed(1)} ms`;
  const lines = [
    hudLine(host, `  ${status}  ${codec}${sharpMode === 'off' ? '' : ` · ${d.holding ? '4:4:4' : sharpMode}`}  ${videoSizes.filter(Boolean).join(' + ')}`),
    hudLine('latency ', latency, ` (max ${d.latencyMax.toFixed(1)})  rtt ${d.rtt.toFixed(1)} ms`),
    hudLine(
      `${d.fps} fps  ${d.mbps.toFixed(1)} Mbps` + (targetMbps ? ` (target ${targetMbps.toFixed(0)})` : '') + `  dropped ${d.dropped}`,
    ),
  ];
  if (d.audioPackets) lines.push(hudLine(`audio ${d.audioPackets} pkt/s  lost ${d.audioLost}  ${d.audioCodec}`));
  hud.replaceChildren(...lines);
}

async function loadMachines() {
  ui.renderMachines(undefined, currentHost);
  try {
    const { machines, groups = [] } = await fetch('machines.json').then((r) => r.json());
    machineNames = Object.fromEntries(machines.map((m) => [machineId(m), m.name]));
    ui.renderMachines(machines.map((m) => ({ ...m, id: machineId(m) })), currentHost, groups);
  } catch {
    ui.renderMachines([], currentHost);
  }
}

function switchMachine(host, name = host) {
  currentHost = host;
  machineFailures = 0;
  storage((s) => (host === homeHost ? s.removeItem(MACHINE_KEY) : s.setItem(MACHINE_KEY, host)));
  displays = [];
  showStatus('connecting', `Connecting to ${name}`);
  armMachineTimeout();
  worker.postMessage({ type: 'switchHost', url: wtUrl(host) });
  log('info', 'switch machine', { host });
  ui.toast({ text: `Connecting to ${name}…`, timeout: 2500 });
}

/** If tracks were negotiated but no picture comes, go back to data-channel video. */
let rtpCheck;
function checkRtpVideoArrives() {
  clearTimeout(rtpCheck);
  rtpCheck = setTimeout(() => {
    if (!rtpVideo || videos.some((v) => v.videoWidth > 0)) return;
    log('warn', 'remote video: no picture over RTP; falling back to the data channel');
    try { sessionStorage.setItem('glass.rtp', '0'); } catch {}
    location.reload();
  }, 6000);
}

// Video numbers over RTP, from the connection's stats (once a second, no layout reads).
const rtpStats = {};
let lastInbound;
setInterval(async () => {
  if (!rtpVideo || !rtc) return;
  const report = await rtc.stats?.();
  if (!report) return;
  let frames = 0, bytes = 0, dropped = 0, jitter = 0, emitted = 0, decode = 0, decoded = 0, rtt;
  for (const r of report.values()) {
    if (r.type === 'inbound-rtp' && r.kind === 'video') {
      frames += r.framesDecoded ?? 0;
      bytes += r.bytesReceived ?? 0;
      dropped += (r.framesDropped ?? 0) + (r.freezeCount ?? 0);
      jitter += r.jitterBufferDelay ?? 0;
      emitted += r.jitterBufferEmittedCount ?? 0;
      decode += r.totalDecodeTime ?? 0;
      decoded += r.framesDecoded ?? 0;
    } else if (r.type === 'candidate-pair' && r.nominated && r.currentRoundTripTime != null) {
      rtt = r.currentRoundTripTime * 1000;
    }
  }
  const now = { frames, bytes, dropped, jitter, emitted, decode, decoded };
  const was = lastInbound ?? now;
  lastInbound = now;
  const dEmitted = emitted - was.emitted;
  const dDecoded = decoded - was.decoded;
  // One-way network time + jitter buffer + decode: what the viewer waits beyond capture.
  const buffer = dEmitted > 0 ? ((jitter - was.jitter) / dEmitted) * 1000 : 0;
  const decodeMs = dDecoded > 0 ? ((decode - was.decode) / dDecoded) * 1000 : 0;
  const latency = (rtt ?? 0) / 2 + buffer + decodeMs;
  Object.assign(rtpStats, {
    fps: frames - was.frames,
    mbps: ((bytes - was.bytes) * 8) / 1e6,
    dropped: dropped - was.dropped,
    latencyAvg: latency,
    latencyMax: latency,
    ...(rtt != null && { rtt }),
    jitterBufferMs: buffer,
  });
}, 1000);

function queueViewport(force = false) {
  if (force) lastViewport = '';
  clearTimeout(viewportTimer);
  viewportTimer = setTimeout(sendViewport, 300);
}
function sendViewport() {
  const elements = rtpVideo ? videos : canvases;
  const streams = [];
  for (let i = 0; i < streamCount; i++) {
    // Keyboard and mouse mode shows no picture: ask for the least, so it never raises the
    // size another viewer gets (the largest request wins).
    if (padOn) {
      streams.push([1, 1]);
      continue;
    }
    // Before the first layout: this window, near enough for the first picture's size.
    const area = areas.get(elements[i])?.[0] ? areas.get(elements[i]) : firstViewport ? [innerWidth, innerHeight] : undefined;
    if (resolutionMode !== 'auto' || !area || !area[0]) {
      streams.push(null);
      continue;
    }
    const scale = devicePixelRatio * zoomScale * pressureScale;
    streams.push([Math.round(area[0] * scale), Math.round(area[1] * scale)]);
  }
  firstViewport = false;
  const msg = JSON.stringify(streams);
  if (msg === lastViewport) return;
  lastViewport = msg;
  control({ t: 'viewport', streams });
}

// "Use as a second display": the host adds a display in this screen's shape beside its main
// one and streams it here. `extended` keeps what was shown before, to go back to it.
let extended;
function setExtended(on) {
  if (on && !extended) {
    extended = { before: streamIds.length > 1 ? 'all' : streamIds[0] };
    const dpr = devicePixelRatio;
    const size = [Math.round(innerWidth * dpr), Math.round(innerHeight * dpr)];
    control({ t: 'extend', size, hidpi: dpr >= 1.5 });
    log('info', 'second display', { size, dpr });
  } else if (!on && extended) {
    const { before } = extended;
    extended = undefined;
    control({ t: 'extend', size: null });
    if (before === 'all') control({ t: 'showAll' });
    else if (before != null) control({ t: 'selectDisplay', id: before });
    log('info', 'second display off');
  }
  ui.setExtend({ on: Boolean(extended) });
}

function setResolution(mode) {
  const wasFit = resolutionMode.startsWith('fit:');
  resolutionMode = mode;
  if (mode.startsWith('fit:')) {
    try { sessionStorage.setItem('glass.fit', mode); } catch {}
    sendFit();
  } else {
    try { sessionStorage.removeItem('glass.fit'); } catch {}
    storage((s) => s.setItem(RESOLUTION_KEY, mode));
    if (wasFit && !guest) control({ t: 'fit', size: null });
  }
  ui.setResolution(mode);
  queueViewport(true);
  log('info', `resolution: ${mode}`);
}

function sendFit() {
  const [w, h] = resolutionMode.slice(4).split('x').map(Number);
  if (w && h) control({ t: 'fit', size: [w, h] });
}

// Turning the device while fitted: the same share of the screen, the other way round (the
// host resizes the display it made, so windows stay on it).
let turnTimer;
addEventListener('resize', () => {
  clearTimeout(turnTimer);
  turnTimer = setTimeout(() => {
    if (!resolutionMode.startsWith('fit:') || guest) return;
    const [w, h] = resolutionMode.slice(4).split('x').map(Number);
    if (!w || !h || (w > h) === (innerWidth > innerHeight)) return;
    const sizes = fitSizes();
    const near = (s) => Math.abs(s[0] * s[1] - w * h);
    const [nw, nh] = sizes.length ? sizes.reduce((a, b) => (near(b) < near(a) ? b : a)) : [h, w];
    log('info', 'fit: turned', { from: [w, h], to: [nw, nh] });
    setResolution(`fit:${nw}x${nh}`);
  }, 400);
});

/**
 * Desktop sizes in this device's shape (its viewing area, in device pixels, at full, ¾ and
 * ½ size), when that shape differs from the Mac display's. Empty when they already match.
 */
function fitSizes() {
  const native = displays.find((d) => d.main) ?? displays[0];
  if (!native) return [];
  const dpr = devicePixelRatio;
  const w = Math.round(innerWidth * dpr);
  const h = Math.round(innerHeight * dpr);
  const mine = w / h;
  const theirs = native.width / native.height;
  if (Math.abs(mine - theirs) < theirs * 0.03) return [];
  const even = (n) => Math.round(n / 2) * 2;
  return [1, 0.75, 0.5]
    .map((f) => [even(w * f), even(h * f)])
    .filter(([a, b]) => Math.max(a, b) >= 1024 && Math.min(a, b) >= 640 && a <= 7680 && b <= 4320);
}

function onLayout(streams) {
  annotator.invalidate();
  input.setStreams(streams);
  input.refreshCursor();
  cursor.setStreams(streams);
  for (const d of streams) if (!streamedWindows.has(d.id) && !displays.some((x) => x.id === d.id)) displays.push(d);
  // The host orders streams as arranged in macOS; lay them out the same way.
  screens.dataset.arrange = arrangement(streams);
  videoSizes.length = streams.length;
  showStreams(streams.length);
  ui.renderDisplays(displays, streams.map((d) => d.id));
  streamIds = streams.map((d) => d.id);
  thumbs.setDisplays(displays.filter((d) => !streamedWindows.has(d.id)), streams.map((d) => d.id));
}

/** 'row' if the displays sit side by side in macOS, 'column' if stacked. */
function arrangement(list) {
  if (list.length < 2) return 'row';
  const spread = (key) => Math.max(...list.map((d) => d[key])) - Math.min(...list.map((d) => d[key]));
  return spread('x') >= spread('y') ? 'row' : 'column';
}

let updateOffered = false;
/** "6:40 PM" today, "Tue 6:40 PM" this week, else "Oct 28, 6:40 PM". */
function formatExpiry(unixSecs) {
  const d = new Date(unixSecs * 1000);
  const days = (d - Date.now()) / 86400e3;
  const opts = days < 1 && d.getDate() === new Date().getDate()
    ? { hour: 'numeric', minute: '2-digit' }
    : days < 6 ? { weekday: 'short', hour: 'numeric', minute: '2-digit' }
    : { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' };
  return d.toLocaleString(undefined, opts);
}

function offerUpdate({ current, version }) {
  if (updateOffered || guest) return;
  updateOffered = true;
  ui.toast({
    text: `Update available: Glass ${version} (you have ${current})`,
    icon: 'update',
    timeout: 0,
    action: {
      label: 'Update',
      run: () => {
        control({ t: 'update' });
        log('info', 'update requested', { from: current, to: version });
        ui.toast({ text: `Updating to ${version}… Glass reconnects when it's done.`, timeout: 15_000 });
      },
    },
  });
}

/** Sends files to the computer: into Downloads, or `dir` (the file browser's folder). */
function uploadFiles(list, dir) {
  for (const file of list) worker.postMessage({ type: 'sendFile', file, dir });
}

function onFileProgress({ id, name, done, total, direction }) {
  let t = transfers.get(id);
  const verb = direction === 'up' ? 'Sending' : 'Receiving';
  // `total` null: a folder zipped as it comes, so no total (an indeterminate bar).
  const fraction = total === null ? null : total ? done / total : 1;
  if (!t) {
    t = ui.toast({ text: `${verb} ${name}`, progress: fraction, timeout: 0 });
    transfers.set(id, t);
  }
  t.update({ progress: fraction, text: total === null ? `${verb} ${name} · ${formatBytes(done)}` : undefined });
  if (done >= total && direction === 'up') {
    t.update({ text: `Sent ${name}` });
    t.dismiss(1500);
    transfers.delete(id);
  }
}

function onFileReceived({ id, file }) {
  transfers.get(id)?.dismiss();
  transfers.delete(id);
  // Saving needs a user gesture on iPad, so offer a button.
  ui.toast({
    text: `${file.name} (${formatBytes(file.size)})`,
    icon: 'file',
    timeout: 0,
    action: { label: 'Save', run: () => saveFile(file) },
  });
}

async function saveFile(file) {
  // Desktop Chromium: a real Save dialog (pick the folder and name).
  if (!mobile && window.showSaveFilePicker) {
    try {
      const handle = await showSaveFilePicker({ suggestedName: file.name });
      const out = await handle.createWritable();
      await out.write(file);
      await out.close();
      return;
    } catch (e) {
      if (e.name === 'AbortError') return;
    }
  }
  if (navigator.canShare?.({ files: [file] })) {
    try {
      await navigator.share({ files: [file] });
      return;
    } catch (e) {
      if (e.name === 'AbortError') return;
    }
  }
  const url = URL.createObjectURL(file);
  const a = document.createElement('a');
  a.href = url;
  a.download = file.name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 60_000);
}

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

let audioContext;
function toggleAudio() {
  if (!audioContext) {
    // Must be created and resumed inside the tap: Safari can start it suspended.
    // 'playback' keeps it audible when the iPad's silent mode is on.
    setAudioSession(true);
    audioContext = new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
    audioContext.resume();
    audioContext.audioWorklet
      .addModule('audio-worklet.js')
      .then(() => {
        const node = new AudioWorkletNode(audioContext, 'glass-audio', { outputChannelCount: [2] });
        node.connect(audioContext.destination);
        worker.postMessage({ type: 'audioPort', port: node.port }, [node.port]);
        worker.postMessage({ type: 'audioToggle' });
        setMediaSession();
        log('info', 'audio started', { sampleRate: audioContext.sampleRate, baseLatency: audioContext.baseLatency });
      })
      .catch((e) => log('error', `audio init failed: ${e.message ?? e}`));
    return;
  }
  if (audioContext.state === 'suspended') audioContext.resume();
  worker.postMessage({ type: 'audioToggle' });
}

// The viewer's microphone, as a microphone on the host.
let micReady = false;
let micInstallable = false;
async function toggleMic() {
  if (micStream) return stopMic();
  if (!micReady) {
    ui.toast({
      text: `Your microphone on ${machineName(currentHost)} needs a small audio driver. Installing it asks for that computer's password, on its screen.`,
      timeout: 0,
      action: {
        label: 'Install',
        run: () => {
          control({ t: 'micInstall' });
          ui.toast({ text: 'Asking for the password on the computer\'s screen. The mic is ready once it\'s typed.', timeout: 8000 });
        },
      },
    });
    return;
  }
  try {
    setAudioSession(Boolean(audioContext), true);
    micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    micContext ??= new AudioContext({ sampleRate: 48000, latencyHint: 'interactive' });
    await micContext.resume();
    await micContext.audioWorklet.addModule('mic-worklet.js');
    const node = new AudioWorkletNode(micContext, 'glass-mic', { numberOfOutputs: 0 });
    micContext.createMediaStreamSource(micStream).connect(node);
    worker.postMessage({ type: 'micPort', port: node.port }, [node.port]);
    worker.postMessage({ type: 'mic', on: true });
    ui.setMicOn(true);
    log('info', 'microphone on', { sampleRate: micContext.sampleRate });
  } catch (e) {
    micStream?.getTracks().forEach((t) => t.stop());
    micStream = undefined;
    setAudioSession();
    ui.toast({ text: `Couldn't use the microphone: ${e.message ?? e}`, kind: 'error' });
    log('error', `microphone failed: ${e.message ?? e}`);
  }
}

// Safari (iOS/iPadOS): 'playback' keeps the Mac's sound audible in silent mode but blocks
// the microphone; while the mic is on it needs 'play-and-record'. Set before each start.
function setAudioSession(playing = Boolean(audioContext), recording = Boolean(micStream)) {
  if (!navigator.audioSession) return;
  navigator.audioSession.type = recording ? 'play-and-record' : playing ? 'playback' : 'auto';
}

function stopMic() {
  micStream?.getTracks().forEach((t) => t.stop());
  micStream = undefined;
  setAudioSession();
  worker.postMessage({ type: 'mic', on: false });
  ui.setMicOn(false);
  log('info', 'microphone off');
}

function toggleStats() {
  statsVisible = !statsVisible;
  hud.classList.toggle('hidden', !statsVisible);
  ui.setStatsPressed(statsVisible);
  storage((s) => s.setItem(STATS_KEY, statsVisible ? '1' : '0'));
}

async function toggleFullscreen() {
  if (document.fullscreenElement) {
    navigator.keyboard?.unlock();
    await document.exitFullscreen();
    return;
  }
  await document.documentElement.requestFullscreen?.();
  // Chromium only: lets Cmd+W and similar shortcuts reach the remote Mac.
  await navigator.keyboard?.lock?.().catch(() => {});
}

// iPadOS suspends background tabs; reconnect as soon as we're back.
let pauseTimer;
document.addEventListener('visibilitychange', () => {
  log('info', `visibility: ${document.visibilityState}`);
  clearTimeout(pauseTimer);
  if (document.visibilityState === 'visible') {
    worker.postMessage({ type: 'visible', visible: !padOn });
    worker.postMessage({ type: 'wake' });
    reloadIfUpdated('visible');
    return;
  }
  // Floating in picture in picture: still on screen, so keep the video going.
  if (floating()) return;
  const pause = () => worker.postMessage({ type: 'visible', visible: false });
  // Synchronously on mobile: the page may be frozen before a timer could run.
  if (mobile) pause();
  else pauseTimer = setTimeout(pause, PAUSE_HIDDEN_AFTER_MS);
});
function floating() {
  return Boolean(document.pictureInPictureElement || window.documentPictureInPicture?.window);
}
// Back from picture in picture with the page still hidden: pause now.
document.addEventListener('leavepictureinpicture', () => {
  if (document.visibilityState === 'hidden') worker.postMessage({ type: 'visible', visible: false });
}, true);
addEventListener('online', () => worker.postMessage({ type: 'wake' }));
// Leaving (a reload included): end the session now. Safari can keep a dead page's
// connection open otherwise, and the host would list this device twice.
addEventListener('pagehide', () => worker.postMessage({ type: 'leave' }));
addEventListener('pageshow', (e) => e.persisted && worker.postMessage({ type: 'wake' }));

// The browser's media controls (Chrome's media hub, the lock screen, media keys) show what
// the computer is playing (its title, artist and artwork) and control it there. Without a
// track they show the computer, and play/pause turn Glass's sound on and off.
let nowPlaying = null; // { title, artist, album, playing, rate, duration, elapsed, at }
let artworkUrl;
function onNowPlaying(info) {
  if (info?.artwork) {
    if (artworkUrl) URL.revokeObjectURL(artworkUrl);
    const bytes = Uint8Array.from(atob(info.artwork), (c) => c.charCodeAt(0));
    artworkUrl = URL.createObjectURL(new Blob([bytes], { type: info.artworkType || 'image/jpeg' }));
  } else if (info && info.artworkId === '' && artworkUrl) {
    URL.revokeObjectURL(artworkUrl);
    artworkUrl = undefined;
  }
  nowPlaying = info && (info.title || info.artist) ? { ...info, receivedAt: Date.now() } : null;
  setMediaSession();
}

function setMediaSession() {
  if (!('mediaSession' in navigator)) return;
  const ms = navigator.mediaSession;
  const handle = (action, fn) => { try { ms.setActionHandler(action, fn); } catch {} };
  const np = nowPlaying;
  if (np) {
    ms.metadata = new MediaMetadata({
      title: np.title ?? '',
      artist: np.artist ?? '',
      album: np.album ?? '',
      artwork: artworkUrl ? [{ src: artworkUrl, sizes: '600x600' }] : [{ src: 'icon-512.png', sizes: '512x512', type: 'image/png' }],
    });
    ms.playbackState = np.playing ? 'playing' : 'paused';
    if (np.duration > 0 && np.elapsed != null) {
      // Where it is now: the reported position, moved on by the time since it was reported.
      const since = np.playing ? (Date.now() - (np.at ?? np.receivedAt)) / 1000 * (np.rate || 1) : 0;
      try {
        ms.setPositionState({ duration: np.duration, position: Math.min(np.duration, Math.max(0, np.elapsed + since)), playbackRate: np.rate > 0 ? np.rate : 1 });
      } catch {}
    }
    const media = (action, position) => () => control({ t: 'media', action, ...(position != null && { position }) });
    handle('play', media('play'));
    handle('pause', media('pause'));
    handle('nexttrack', media('next'));
    handle('previoustrack', media('previous'));
    handle('seekto', (d) => d.seekTime != null && control({ t: 'media', action: 'seek', position: d.seekTime }));
    return;
  }
  ms.metadata = new MediaMetadata({
    title: machineName(currentHost),
    artist: 'Glass',
    artwork: [{ src: 'icon-512.png', sizes: '512x512', type: 'image/png' }],
  });
  const running = audioContext?.state === 'running';
  ms.playbackState = running ? 'playing' : 'paused';
  for (const [action, on] of [['play', true], ['pause', false]]) {
    handle(action, () => {
      if (on === (audioContext?.state === 'running')) return;
      toggleAudio();
      ms.playbackState = on ? 'playing' : 'paused';
    });
  }
  for (const action of ['nexttrack', 'previoustrack', 'seekto']) handle(action, null);
}

// A new network (Wi-Fi to cellular, a VPN coming up): reconnect now instead of waiting.
navigator.connection?.addEventListener?.('change', () => {
  const c = navigator.connection;
  log('info', 'network changed', { type: c.type, effectiveType: c.effectiveType, downlink: c.downlink, rtt: c.rtt });
  worker.postMessage({ type: 'wake' });
});

// One viewer per browser: a second tab of this computer would be a second session (and
// fight the first over the stream). The newest tab asks before taking over. Windows
// opened for "use all my screens" each show their own display, so they don't count.
if (navigator.locks && !displayParam) {
  const name = `glass-viewer:${remoteKey ?? homeHost}`;
  let waiting;
  const away = (title, text) => {
    elsewhere = [title, 'Glass is showing this computer in another tab of this browser.'];
    worker.postMessage({ type: 'suspend' });
    onConnectionStatus('elsewhere');
    waiting = ui.toast({ text, timeout: 0, action: { label: 'Use it here', run: () => hold(true) } });
  };
  const hold = (steal) =>
    navigator.locks.request(name, steal ? { steal: true } : { ifAvailable: true }, async (lock) => {
      if (!lock) return away('Open in another tab', 'Glass is open in another tab.');
      waiting?.dismiss();
      if (elsewhere) {
        elsewhere = undefined;
        showStatus('connecting', 'Connecting', machineName(currentHost));
        worker.postMessage({ type: 'resume' });
      }
      await new Promise(() => {}); // held until the tab closes or another takes it
    }).catch((e) => {
      if (e.name === 'AbortError') away('Moved to another tab', 'Glass moved to another tab.');
    });
  hold(false);
}

// Files opened with Glass (File Handling: "Open with" on the desktop), sent once connected.
let pendingFiles = [];
if ('launchQueue' in window) {
  launchQueue.setConsumer(async (params) => {
    const files = await Promise.all((params.files ?? []).map((h) => h.getFile()));
    if (params.targetURL) {
      const open = new URL(params.targetURL).searchParams.get('open');
      if (open && open !== 'files') runOpen(open);
    }
    if (!files.length) return;
    if (session) uploadFiles(files);
    else pendingFiles.push(...files);
  });
}

/** A shortcut from the app icon: open chat, the machines list, or send files. */
function runOpen(what) {
  if (what === 'chat') chat.open();
  else if (what === 'machines') ui.openMenu('machines');
  else if (what === 'send') {
    // Choosing files needs a tap.
    ui.toast({ text: 'Send files to the Mac', icon: 'file', timeout: 0, action: { label: 'Choose…', run: () => document.getElementById('file-input').click() } });
  }
}

let welcomed = false;
async function onFirstWelcome() {
  if (displayParam) control({ t: 'selectDisplay', id: Number(displayParam) });
  if (welcomed) return;
  welcomed = true;
  if (openParam && openParam !== 'files') runOpen(openParam);
  if (pendingFiles.length) {
    uploadFiles(pendingFiles);
    pendingFiles = [];
  }
  // Shared to Glass from another app (Web Share Target), stashed by the service worker.
  if (sharedParam && 'caches' in window) {
    const cache = await caches.open('glass-share');
    const files = [];
    for (const req of await cache.keys()) {
      const r = await cache.match(req);
      const path = new URL(req.url).pathname;
      if (path.endsWith('/shared/text')) {
        const text = await r.text();
        control({ t: 'clipboard', text });
        ui.toast({ text: `On ${machineName(currentHost)}'s clipboard: ${text.length > 60 ? `${text.slice(0, 59)}…` : text}`, timeout: 5000 });
      } else if (path.includes('/shared/file/')) {
        const blob = await r.blob();
        files.push(new File([blob], decodeURIComponent(r.headers.get('x-name') ?? 'file'), { type: blob.type }));
      }
    }
    await caches.delete('glass-share');
    if (files.length) uploadFiles(files);
  }
}

// Trackpad mode (touch screens): on by default on phones, where fingers are big next to
// what they'd tap; remembered per device.
const TRACKPAD_KEY = 'glass.trackpad';
const touchScreen = matchMedia('(hover: none)').matches || navigator.maxTouchPoints > 0;
const phone = touchScreen && Math.min(screen.width, screen.height) < 600;
let padOn = false; // keyboard and mouse mode: see setPad
let trackpadOn = touchScreen && (storage((s) => s.getItem(TRACKPAD_KEY)) ?? (phone ? '1' : '0')) === '1';
function setTrackpad(on, chosen = false) {
  trackpadOn = on;
  input.setTrackpad(on);
  // Trackpad mode draws its own pointer (#tp-cursor): the host's goes back in the video.
  cursor.setWanted(!on);
  ui.setTrackpad({ available: touchScreen, on });
  if (!chosen) return;
  storage((s) => s.setItem(TRACKPAD_KEY, on ? '1' : '0'));
  log('info', `trackpad mode ${on ? 'on' : 'off'}`);
  ui.toast({
    text: on
      ? 'Trackpad: drag to move the pointer, tap to click, two-finger tap to right-click, double-tap and hold to drag, two fingers to scroll, pinch to zoom.'
      : 'Touch: tap where you want to click.',
    timeout: on ? 7000 : 2500,
  });
}
setTrackpad(trackpadOn);

// Keyboard and mouse mode (touch screens, from the Computer menu): another screen shows the
// computer (a car, a TV) and this one is only its trackpad and keyboard. The picture is
// paused here (the host stops sending it) and hidden, and the whole screen is the trackpad.
let padWasTrackpad = false;
function setPad(on, chosen = false) {
  if (padOn === on) return;
  padOn = on;
  document.body.classList.toggle('pad', on);
  if (on) {
    padWasTrackpad = trackpadOn;
    setTrackpad(true);
  } else {
    setTrackpad(padWasTrackpad);
  }
  worker.postMessage({ type: 'visible', visible: !on && document.visibilityState === 'visible' });
  sendViewport();
  ui.setPad({ available: touchScreen, on });
  log('info', `keyboard and mouse mode ${on ? 'on' : 'off'}`);
  if (chosen && on) ui.toast({ text: 'This screen is the trackpad now. Tap the keyboard button to type.', timeout: 5000 });
}
document.getElementById('pad-exit').addEventListener('click', () => setPad(false, true));
ui.setPad({ available: touchScreen, on: false });
