// Toolbar, panels, toasts and the token prompt. DOM writes only; no layout reads.

import { CUSTOM_KEY, comboFromEvent, parseCustom, serializeCustom } from './shortcuts.js';
import { droppedFiles } from './device.js';

const $ = (id) => document.getElementById(id);
// localStorage can throw (private browsing, storage blocked).
const storageGet = (key) => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const storageSet = (key, value) => {
  try {
    localStorage.setItem(key, value);
  } catch {}
};


export function createUI({ onOpenMachines, onSelectMachine, onWakeMachine = () => {}, onSelectDisplay, onSharp, onListWindows, onSelectWindow, onShortcut, shortcuts, swapCmdOption, onSwapCmdOption, ctrlAsCmd, onCtrlAsCmd, pcKeyboard, onPcKeyboard, onPhysicalKeyboard, onKeyboard, onUploadFiles, onRequestFiles, onAudio, onMic, onFullscreen, onToggleStats, onPrivacy, onRestart, onSas, onToken, onTerminal = () => {}, onOpenDevice = () => {}, onTrackpad = () => {}, onPad = () => {}, onExtend = () => {}, resolution = 'auto', onResolution, fitSizes = () => [], deviceName = () => 'device' }) {
  const toolbar = $('toolbar');
  // Each toolbar menu: its button and panel.
  const menus = {
    machines: { button: $('btn-machines'), panel: $('machines-panel') },
    displays: { button: $('btn-displays'), panel: $('displays-panel') },
    shortcuts: { button: $('btn-shortcuts'), panel: $('shortcuts-panel') },
    more: { button: $('btn-more'), panel: $('more-panel') },
    ports: { button: $('btn-ports'), panel: $('ports-panel') },
    device: { button: $('btn-device'), panel: $('device-panel') },
  };
  const fileInput = $('file-input');
  const toasts = $('toasts');
  const auth = $('auth');

  // Hidden only when asked (its last button); a small tab at the top brings it back.
  const TOOLBAR_HIDDEN_KEY = 'glass.toolbarHidden';
  const toolbarShow = $('toolbar-show');
  // Tapping the toolbar (or its tab) mustn't take focus from the field the on-screen
  // keyboard types into: iOS would put the keyboard away (hiding the toolbar did).
  const keys = $('keys');
  for (const el of [toolbar, $('toolbar-show')]) {
    el.addEventListener('mousedown', (e) => {
      if (document.activeElement === keys && !e.target.closest('input, textarea')) e.preventDefault();
    });
  }
  function setToolbarHidden(hide) {
    const typing = document.activeElement === keys;
    toolbar.classList.toggle('tucked', hide);
    toolbar.inert = hide;
    toolbarShow.classList.toggle('hidden', !hide);
    if (hide) for (const name of Object.keys(menus)) setMenuOpen(name, false);
    // Keep typing: still in the tap, so focusing again keeps the keyboard up.
    if (typing && document.activeElement !== keys) keys.focus({ preventScroll: true });
    storageSet(TOOLBAR_HIDDEN_KEY, hide ? '1' : '0');
  }
  // In the title bar (an installed desktop app) there's nothing to tuck away.
  const inTitleBar = matchMedia('(display-mode: window-controls-overlay)').matches;
  if (storageGet(TOOLBAR_HIDDEN_KEY) === '1' && !inTitleBar) {
    toolbar.classList.add('tucked');
    toolbar.inert = true;
    toolbarShow.classList.remove('hidden');
  }
  toolbarShow.addEventListener('click', (e) => {
    e.preventDefault();
    setToolbarHidden(false);
  });
  // Touch or mouse, for sizing things for a fingertip (CSS keys off data-input).
  const root = document.documentElement;
  root.dataset.input = matchMedia('(hover: none)').matches ? 'touch' : 'mouse';
  addEventListener('pointerdown', (e) => {
    const input = e.pointerType === 'touch' ? 'touch' : 'mouse';
    if (root.dataset.input !== input) root.dataset.input = input;
  }, { capture: true, passive: true });

  // Toolbar buttons must not steal pointer events from the canvas logic or keep focus.
  const on = (id, fn) => $(id).addEventListener('click', (e) => { e.preventDefault(); fn(e); });

  on('btn-machines', () => {
    toggleMenu('machines');
    if (!menus.machines.panel.classList.contains('hidden')) onOpenMachines();
  });
  on('btn-displays', () => {
    toggleMenu('displays');
    // Windows come and go, so ask for a fresh list each time the menu opens.
    if (windowsAvailable && !menus.displays.panel.classList.contains('hidden')) onListWindows?.();
  });
  on('btn-shortcuts', () => toggleMenu('shortcuts'));
  on('btn-more', () => toggleMenu('more'));
  on('btn-keyboard', onKeyboard);
  on('btn-upload', () => fileInput.click());
  on('btn-download', onRequestFiles);
  on('btn-audio', onAudio);
  on('btn-mic', () => onMic?.());
  on('btn-fullscreen', onFullscreen);
  on('btn-terminal', onTerminal);
  on('btn-ports', () => toggleMenu('ports'));
  on('btn-hide-toolbar', () => setToolbarHidden(true));
  on('btn-trackpad', () => onTrackpad());

  // Phones: each toolbar group is one button with a dropdown (CSS decides when). A group
  // knows how many of its buttons show, so one with a single button shows it directly.
  const groups = [...toolbar.querySelectorAll('.tb-group')];
  function countGroups() {
    for (const g of groups) {
      // Dropdown labels: the button's title without its shortcut ("Fullscreen (Ctrl+…)").
      for (const b of g.querySelectorAll('.tb-items > button')) {
        const label = (b.title || '').replace(/\s*\(.*\)$/, '');
        if (b.dataset.label !== label) b.dataset.label = label;
      }
      const n = String(g.querySelectorAll('.tb-items > button:not(.hidden)').length);
      if (g.dataset.n !== n) g.dataset.n = n;
    }
  }
  countGroups();
  new MutationObserver(countGroups).observe(toolbar, { subtree: true, childList: true, attributes: true, attributeFilter: ['class', 'title'] });
  function setGroupOpen(group, open) {
    group.classList.toggle('open', open);
    group.querySelector('.tb-trigger')?.setAttribute('aria-expanded', String(open));
  }
  for (const g of groups) {
    g.querySelector('.tb-trigger')?.addEventListener('click', (e) => {
      e.preventDefault();
      const open = !g.classList.contains('open');
      for (const other of groups) setGroupOpen(other, other === g && open);
      if (open) for (const name of Object.keys(menus)) setMenuOpen(name, false);
    });
    // Picking something closes the dropdown (a button that opens a menu opens it after).
    g.querySelector('.tb-items')?.addEventListener('click', () => setGroupOpen(g, false));
  }
  addEventListener('pointerdown', (e) => {
    for (const g of groups) if (g.classList.contains('open') && !g.contains(e.target)) setGroupOpen(g, false);
  }, { capture: true });
  addEventListener('keydown', (e) => {
    if (e.key !== 'Escape') return;
    const open = groups.find((g) => g.classList.contains('open'));
    if (!open) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    setGroupOpen(open, false);
    open.querySelector('.tb-trigger')?.focus();
  }, { capture: true });
  on('btn-device', () => {
    toggleMenu('device');
    if (!menus.device.panel.classList.contains('hidden')) onOpenDevice();
  });

  fileInput.addEventListener('change', () => {
    if (fileInput.files.length) onUploadFiles([...fileInput.files]);
    fileInput.value = '';
  });

  // Drag-and-drop files from the client device onto the remote screen.
  addEventListener('dragover', (e) => e.preventDefault());
  // Folders too: their files keep their paths inside the folder.
  addEventListener('drop', async (e) => {
    e.preventDefault();
    const files = await droppedFiles(e.dataTransfer).catch(() => [...(e.dataTransfer?.files ?? [])]);
    if (files.length) onUploadFiles(files);
  });

  addEventListener('keydown', (e) => {
    // While recording a shortcut, Escape cancels the recording instead.
    if (e.key !== 'Escape' || recording) return;
    const open = Object.keys(menus).find((name) => !menus[name].panel.classList.contains('hidden'));
    if (!open) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    setMenuOpen(open, false);
    menus[open].button.focus();
  }, { capture: true });

  function setMenuOpen(name, open) {
    const { button, panel } = menus[name];
    panel.classList.toggle('hidden', !open);
    button.setAttribute('aria-expanded', String(open));
  }
  function toggleMenu(name) {
    for (const other of Object.keys(menus)) {
      setMenuOpen(other, other === name && menus[name].panel.classList.contains('hidden'));
    }
  }
  addEventListener('pointerdown', (e) => {
    for (const [name, { button, panel }] of Object.entries(menus)) {
      if (!panel.contains(e.target) && !button.contains(e.target)) setMenuOpen(name, false);
    }
  }, { capture: true });

  function menuItem(label, meta, { checked, onSelect }) {
    const b = document.createElement('button');
    b.setAttribute('role', checked === undefined ? 'menuitem' : 'menuitemradio');
    if (checked !== undefined) b.setAttribute('aria-checked', String(checked));
    const l = document.createElement('span');
    l.textContent = label;
    const m = document.createElement('span');
    m.className = 'meta';
    m.textContent = meta;
    b.append(l, m);
    b.addEventListener('click', onSelect);
    return b;
  }

  function toggleItem(label, meta, initial, onChange) {
    let on = initial;
    const item = menuItem(label, meta, {
      checked: on,
      onSelect: () => {
        on = !on;
        item.setAttribute('aria-checked', String(on));
        onChange(on);
      },
    });
    // Set from outside (a machine's keyboard profile) without calling back.
    item.set = (v) => {
      on = v;
      item.setAttribute('aria-checked', String(on));
    };
    return item;
  }
  const pcItem = toggleItem('PC keyboard', 'Ctrl → ⌘, Super → ⌃', pcKeyboard, onPcKeyboard);
  const ctrlItem = toggleItem('Ctrl as ⌘', '⌃⇥ ⌃Space ⌃H ⌃⇧3–5', ctrlAsCmd, onCtrlAsCmd);

  let swapOn = swapCmdOption;
  const swapItem = menuItem('Swap ⌘ and ⌥', 'see note', {
    checked: swapOn,
    onSelect: () => {
      swapOn = !swapOn;
      swapItem.setAttribute('aria-checked', String(swapOn));
      onSwapCmdOption(swapOn);
    },
  });
  swapItem.set = (v) => {
    swapOn = v;
    swapItem.setAttribute('aria-checked', String(swapOn));
  };
  swapItem.title = 'Pair with iPad Settings › General › Keyboard › Hardware Keyboard › Modifier Keys: Command → Option, Option → Command. Lets ⌘Tab, ⌘Space and ⌘H reach the Mac.';
  const note = document.createElement('p');
  note.className = 'note';
  note.textContent = 'iPadOS keeps ⌘Tab, ⌘Space, ⌘H and ⌘⇧3–5. “Ctrl as ⌘” sends those combos when you press Ctrl instead. Alternatively swap Command and Option in iPad Settings › Keyboard › Hardware Keyboard › Modifier Keys and turn on “Swap ⌘ and ⌥”.';
  const divider = document.createElement('hr');

  const shortcutItem = (sc) =>
    menuItem(sc.label, sc.keysLabel, {
      onSelect: () => {
        setMenuOpen('shortcuts', false);
        onShortcut(sc);
      },
    });
  // Your own shortcuts: recorded from the next combo pressed, kept in localStorage.
  let custom = parseCustom(storageGet(CUSTOM_KEY));
  let recording = false;
  function saveCustom() {
    storageSet(CUSTOM_KEY, serializeCustom(custom));
    renderShortcuts();
  }
  function customRow(sc, i) {
    const row = document.createElement('div');
    row.style.cssText = 'display:flex;align-items:center;gap:2px';
    const item = shortcutItem(sc);
    item.style.flex = '1';
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.textContent = '×';
    remove.title = `Remove ${sc.label}`;
    remove.setAttribute('aria-label', `Remove ${sc.label}`);
    remove.style.cssText = 'width:auto;flex:none;padding:10px 12px;color:var(--text-dim)';
    remove.addEventListener('click', (e) => {
      e.stopPropagation();
      custom = custom.filter((_, j) => j !== i);
      saveCustom();
    });
    row.append(item, remove);
    return row;
  }
  function addShortcut() {
    if (recording) return;
    recording = true;
    renderShortcuts();
    // Capture phase on window: runs before the input handler, so the combo isn't sent.
    const onKey = (e) => {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.code === 'Escape' && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey) return finish(null);
      const combo = comboFromEvent(e);
      if (combo) finish(combo);
    };
    const swallowUp = (e) => e.stopImmediatePropagation();
    function finish(combo) {
      removeEventListener('keydown', onKey, true);
      setTimeout(() => removeEventListener('keyup', swallowUp, true), 300);
      recording = false;
      if (combo) {
        const label = (prompt('Name this shortcut', combo.keysLabel) ?? '').trim();
        if (label) custom = [...custom, ...parseCustom(serializeCustom([{ label, codes: combo.codes }]))];
      }
      saveCustom();
    }
    addEventListener('keydown', onKey, true);
    addEventListener('keyup', swallowUp, true);
  }
  // A bar of shortcut buttons along the bottom, for touch screens without a keyboard.
  const BAR_KEY = 'glass.shortcutBar';
  let barOn = storageGet(BAR_KEY) === '1';
  const bar = document.createElement('div');
  bar.id = 'shortcut-bar';
  bar.className = 'glass hidden';
  bar.setAttribute('role', 'toolbar');
  bar.setAttribute('aria-label', 'Shortcut bar');
  document.body.append(bar);
  const barItem = toggleItem('Shortcut bar', 'buttons along the bottom', barOn, (on) => {
    barOn = on;
    storageSet(BAR_KEY, on ? '1' : '0');
    renderBar();
  });
  function renderBar() {
    bar.classList.toggle('hidden', !barOn || $('btn-shortcuts').classList.contains('hidden'));
    bar.replaceChildren(...[...shortcuts, ...custom].map((sc) => {
      const b = document.createElement('button');
      b.type = 'button';
      b.textContent = sc.keysLabel;
      b.title = sc.label;
      b.addEventListener('pointerdown', (e) => e.preventDefault()); // keep keyboard focus
      b.addEventListener('click', (e) => { e.preventDefault(); onShortcut(sc); });
      return b;
    }));
  }
  function renderShortcuts() {
    renderBar();
    const add = menuItem(recording ? 'Press a shortcut…' : 'Add shortcut…', recording ? 'Esc to cancel' : '', { onSelect: addShortcut });
    menus.shortcuts.panel.replaceChildren(
      pcItem,
      ctrlItem,
      swapItem,
      note,
      barItem,
      divider,
      ...shortcuts.map(shortcutItem),
      ...custom.map(customRow),
      add,
    );
  }
  renderShortcuts();

  /** `current` is the id ("host" or "host:port") of the machine this session is on. */
  function renderMachines(machines, current, groups = []) {
    if (!machines) {
      menus.machines.panel.replaceChildren(menuItem('Looking for machines…', '', { onSelect: () => {} }));
      return;
    }
    // An offline machine whose network address Glass has seen can be woken up (Wake-on-LAN).
    const wakeable = (m) => !m.online && m.macs?.length > 0;
    const status = (m) => (m.id === current ? 'connected' : wakeable(m) ? 'offline · wake' : !m.online ? 'offline' : !m.glass ? 'Glass not running' : m.os);
    const items = machines.map((m) => {
      const item = menuItem(m.name, status(m), {
        checked: m.id === current,
        onSelect: () => {
          setMenuOpen('machines', false);
          if (wakeable(m)) onWakeMachine(m);
          else if (m.id !== current) onSelectMachine(m);
        },
      });
      item.disabled = m.id !== current && !wakeable(m) && (!m.online || !m.glass);
      return item;
    });
    if (!items.length) items.push(menuItem('No machines found', 'needs Tailscale', { onSelect: () => {} }));
    // Groups from the console's address book, as headings; the rest after them.
    const groupOf = (m) => groups.find((g) => g.machines.includes(m.dns?.toLowerCase()))?.name;
    if (groups.length && machines.some(groupOf)) {
      const heading = (text) => {
        const p = document.createElement('p');
        p.className = 'note';
        p.textContent = text;
        return p;
      };
      const out = [];
      for (const g of [...groups.map((g) => g.name), undefined]) {
        const members = machines.map((m, i) => [m, items[i]]).filter(([m]) => groupOf(m) === g);
        if (!members.length) continue;
        out.push(heading(g ?? 'Other'), ...members.map(([, item]) => item));
      }
      menus.machines.panel.replaceChildren(...out);
      return;
    }
    menus.machines.panel.replaceChildren(...items);
  }

  /** `streamed` lists the display ids currently shown, in stream order. */
  function renderDisplays(displays, streamed) {
    const all = streamed.length > 1;
    const names = positionNames(displays);
    const ordered = [...displays].sort((a, b) => a.x - b.x || a.y - b.y);
    const items = ordered.map((d) =>
      menuItem(names.get(d.id), `${d.width}×${d.height}${d.main ? ' · main' : ''}`, {
        checked: !all && streamed[0] === d.id,
        onSelect: () => {
          setMenuOpen('displays', false);
          if (all || streamed[0] !== d.id) onSelectDisplay(d.id);
        },
      }),
    );
    if (displays.length > 1) {
      items.push(
        menuItem('All displays', `${displays.length} side by side`, {
          checked: all,
          onSelect: () => {
            setMenuOpen('displays', false);
            if (!all) onSelectDisplay('all');
          },
        }),
      );
    }
    // This screen as one more display of the Mac's, beside its main one.
    if (extendAvailable) {
      items.push(
        document.createElement('hr'),
        menuItem('Use as a second display', extended ? 'on' : 'extends the desktop', {
          checked: extended,
          onSelect: () => { setMenuOpen('displays', false); onExtend(!extended); },
        }),
      );
    }
    // Resolution: sized to this screen (sharper and faster), or the Mac's full size.
    const native = displays.find((d) => d.id === streamed[0]) ?? displays[0];
    items.push(
      document.createElement('hr'),
      menuItem('Auto', 'sized to this screen', {
        checked: resolutionMode === 'auto',
        onSelect: () => { setMenuOpen('displays', false); onResolution?.('auto'); },
      }),
      menuItem(hostOs === 'macos' ? 'This Mac' : 'Full size', native ? `${native.width}×${native.height}` : 'full resolution', {
        checked: resolutionMode === 'native',
        onSelect: () => { setMenuOpen('displays', false); onResolution?.('native'); },
      }),
    );
    // Sharp text: full size, fewer but finer frames (and 4:2:2 colour where it can).
    if (sharpAvailable) {
      items.push(
        document.createElement('hr'),
        menuItem('Sharp text', sharpMode === 'off' ? 'for reading' : sharpMode === '4:2:2' ? 'on · 4:2:2' : 'on', {
          checked: sharpOn,
          onSelect: () => { setMenuOpen('displays', false); onSharp?.(!sharpOn); },
        }),
      );
    }
    // Fit this device: the Mac's desktop in this screen's shape, no black bars.
    const fits = canFit ? fitSizes() : [];
    if (fits.length) {
      const heading = document.createElement('p');
      heading.className = 'note';
      heading.textContent = `Fit this ${deviceName()} · changes the Mac's display shape while you're connected`;
      items.push(document.createElement('hr'), heading);
      for (const [w, h] of fits) {
        const mode = `fit:${w}x${h}`;
        items.push(menuItem(`${w}×${h}`, '', {
          checked: resolutionMode === mode,
          onSelect: () => { setMenuOpen('displays', false); onResolution?.(mode); },
        }));
      }
    }
    // Apps: stream one window on its own.
    if (windowsAvailable) {
      const heading = document.createElement('p');
      heading.className = 'note';
      heading.textContent = 'Apps · show one window on its own';
      items.push(document.createElement('hr'), heading);
      if (!windowList) items.push(menuItem('Looking for windows…', '', { onSelect: () => {} }));
      else if (!windowList.length) items.push(menuItem('No windows', '', { onSelect: () => {} }));
      for (const w of windowList ?? []) {
        const meta = w.title && w.title !== w.app ? w.title : `${w.width}×${w.height}`;
        const item = menuItem(w.app, meta.length > 32 ? `${meta.slice(0, 31)}…` : meta, {
          checked: streamed.length === 1 && streamed[0] === w.id,
          onSelect: () => {
            setMenuOpen('displays', false);
            if (streamed[0] !== w.id) onSelectWindow?.(w.id);
          },
        });
        item.title = `${w.app} — ${w.title}`;
        items.push(item);
      }
    }
    lastDisplays = [displays, streamed];
    menus.displays.panel.replaceChildren(...items);
  }
  let resolutionMode = resolution;
  let lastDisplays;

  // The … menu: stats for everyone; for the owner, the privacy screen, restarts and the
  // console.
  let statsOn = false;
  let isOwner = false;
  let privacyAvailable = false;
  let privacyOn = false;
  let hostOs;
  let canInput = true;
  let physicalKeyboard = false;
  function renderMore() {
    const items = [
      menuItem('Stats', 'Ctrl+Shift+H', {
        checked: statsOn,
        onSelect: () => { setMenuOpen('more', false); onToggleStats?.(); },
      }),
    ];
    if (canInput && hostOs !== 'linux') {
      items.push(menuItem('Physical keyboard', 'Use host layout (games)', {
        checked: physicalKeyboard,
        onSelect: () => {
          physicalKeyboard = !physicalKeyboard;
          onPhysicalKeyboard?.(physicalKeyboard);
          renderMore();
        },
      }));
    }
    // Ctrl+Alt+Del: any input-capable viewer of a Windows host (needs the secure-desktop service).
    if (hostOs === 'windows' && canInput) {
      items.push(menuItem('Send Ctrl+Alt+Del', 'secure attention', {
        onSelect: () => { setMenuOpen('more', false); onSas?.(); },
      }));
    }
    // Another screen shows the computer (a car, a TV); this one is its trackpad and keyboard.
    if (padAvailable) {
      items.push(menuItem('Keyboard and mouse mode', 'the picture is paused here', {
        checked: padActive,
        onSelect: () => { setMenuOpen('more', false); onPad(!padActive); },
      }));
    }
    if (!isOwner) {
      menus.more.panel.replaceChildren(...items);
      return;
    }
    items.push(document.createElement('hr'));
    if (privacyAvailable) {
      const note = document.createElement('p');
      note.className = 'note';
      note.textContent = "Blanks the computer's own screen and ignores its keyboard and mouse while you're connected.";
      items.push(
        menuItem('Privacy screen', privacyOn ? 'on' : 'off', {
          checked: privacyOn,
          onSelect: () => { setMenuOpen('more', false); onPrivacy?.(!privacyOn); },
        }),
        note,
        document.createElement('hr'),
      );
    }
    const restart = (label, meta, glass, question) =>
      menuItem(label, meta, {
        onSelect: () => {
          setMenuOpen('more', false);
          if (confirm(question)) onRestart?.(glass);
        },
      });
    items.push(
      restart('Restart Glass', 'reconnects in seconds', true, "Restart Glass on this computer? You'll reconnect in a few seconds."),
      restart('Restart computer', 'reconnects after login', false, "Restart this computer? Open apps are asked to quit. Glass reconnects once it's back and someone is logged in."),
    );
    if (consoleUrl) {
      items.push(
        document.createElement('hr'),
        menuItem('Console', 'links, people, settings', {
          onSelect: () => {
            setMenuOpen('more', false);
            open(consoleUrl, '_blank', 'noopener');
          },
        }),
      );
    }
    menus.more.panel.replaceChildren(...items);
  }
  let padAvailable = false; // a touch screen: it can be just a trackpad and keyboard
  let padActive = false;
  let consoleUrl; // the owner's console (/admin/), from the Computer menu
  let extendAvailable = false; // owner, and the host can add a display
  let extended = false; // this device is a second display now
  let canFit = true;
  let sharpAvailable = false;
  let sharpOn = false;
  let sharpMode = 'off';
  let windowsAvailable = false;
  let windowList = null;

  /** Names displays by where they sit in the macOS arrangement: Left / Right, Top / Bottom. */

  const ICONS = {
    link: '<path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7"/><path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7"/>',
    info: '<path d="M12 16v-4M12 8h.01"/><circle cx="12" cy="12" r="9"/>',
    error: '<path d="M12 8v4M12 16h.01"/><path d="M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/>',
    file: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5"/>',
    update: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>',
    chat: '<path d="M21 12a8 8 0 0 1-11.6 7.1L4 20.5l1.4-4.6A8 8 0 1 1 21 12z"/>',
  };
  const svg = (paths, cls) =>
    `<svg class="${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${paths}</svg>`;

  function toast({ text, kind, icon, action, timeout = 4000, progress }) {
    const el = document.createElement('div');
    el.className = `toast glass${kind === 'error' ? ' error' : ''}`;
    el.setAttribute('role', kind === 'error' ? 'alert' : 'status');
    el.insertAdjacentHTML('afterbegin', svg(ICONS[icon ?? (kind === 'error' ? 'error' : 'info')], 'icon'));
    const label = document.createElement('span');
    label.className = 'text';
    label.textContent = text;
    label.title = text;
    el.append(label);
    let bar;
    if (progress !== undefined) {
      bar = document.createElement('progress');
      bar.max = 1;
      // null: indeterminate (no total known).
      if (progress !== null) bar.value = progress;
      el.append(bar);
    }
    let timer;
    const remove = () => {
      clearTimeout(timer);
      el.classList.add('leaving');
      setTimeout(() => el.remove(), 180);
    };
    if (action) {
      const b = document.createElement('button');
      b.className = 'action';
      b.textContent = action.label;
      b.addEventListener('click', () => { action.run(); remove(); });
      el.append(b);
    }
    const close = document.createElement('button');
    close.className = 'close';
    close.setAttribute('aria-label', 'Dismiss');
    close.innerHTML = svg('<path d="M6 6l12 12M18 6 6 18"/>', '');
    close.addEventListener('click', remove);
    el.append(close);
    toasts.append(el);
    const api = {
      update({ text: t, progress: p }) {
        if (t !== undefined) label.textContent = label.title = t;
        if (bar && p != null) bar.value = p;
      },
      dismiss(after = 0) {
        clearTimeout(timer);
        timer = setTimeout(remove, after);
      },
    };
    if (timeout) api.dismiss(timeout);
    return api;
  }

  auth.querySelector('form').addEventListener('submit', (e) => {
    e.preventDefault();
    const token = new FormData(e.target).get('token').trim();
    if (!token) return;
    auth.classList.add('hidden');
    onToken(token);
  });

  return {
    renderMachines,
    renderDisplays,
    toast,
    /** `rejected`: a token was tried and refused, so say so. */
    showAuth({ rejected = false } = {}) {
      auth.classList.remove('hidden');
      $('auth-error').classList.toggle('hidden', !rejected);
      const input = auth.querySelector('input');
      input.setAttribute('aria-invalid', String(rejected));
      if (rejected) input.select();
      input.focus();
    },
    /** Guests (signed in with a magic link) can't switch machines. */
    setGuest(guest) {
      $('btn-machines').classList.toggle('hidden', guest);
      if (guest) setMenuOpen('machines', false);
    },
    /** What this session may do: a link can be view only, or without files. */
    setPerms({ input = true, files = true } = {}) {
      canInput = input;
      $('btn-upload').classList.toggle('hidden', !files);
      $('btn-download').classList.toggle('hidden', !files);
      $('btn-keyboard').classList.toggle('hidden', !input);
      if (!input) $('btn-shortcuts').classList.add('hidden');
      renderBar();
      renderMore();
    },
    /** "Use as a second display": offered (`available`) and on (`on`). */
    setExtend({ available = extendAvailable, on = extended } = {}) {
      extendAvailable = available;
      extended = on;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    /** Only the owner can reshape the host's display (guests get an error). */
    setCanFit(on) {
      canFit = on;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    /** Sharp text: `available` (may this viewer turn it on), `on` (its choice), `mode` (the host's). */
    setSharp({ available = sharpAvailable, on = sharpOn, mode = sharpMode } = {}) {
      sharpAvailable = available;
      sharpOn = on;
      sharpMode = mode;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    /** The host can stream single windows (an Apps section in the displays menu). */
    setWindowsAvailable(on) {
      windowsAvailable = on;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    renderWindows(list) {
      windowList = list;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    setResolution(mode) {
      resolutionMode = mode;
      if (lastDisplays) renderDisplays(...lastDisplays);
    },
    /** The … menu: owner items only for the owner; the privacy item only where the host supports it. */
    setComputerControls({ isOwner: owner, privacy, console: url }) {
      privacyAvailable = privacy;
      isOwner = owner;
      consoleUrl = owner ? url : undefined;
      renderMore();
    },
    setPrivacyOn(on) {
      privacyOn = on;
      renderMore();
    },
    setStatsPressed(on) {
      statsOn = on;
      renderMore();
    },
    /** The ⌘ menu (Mac shortcuts and key remaps) only applies to Mac hosts. */
    setHostOs(os) {
      hostOs = os;
      $('btn-shortcuts').classList.toggle('hidden', os !== 'macos');
      renderBar();
      renderMore();
    },
    /** Shows this machine's keyboard remaps in the menu. */
    setKeyboardProfile({ pc, ctrlAsCmd, swap, physical = false }) {
      pcItem.set(pc);
      ctrlItem.set(ctrlAsCmd);
      swapItem.set(swap);
      physicalKeyboard = physical;
      renderMore();
    },
    /** Ports the host forwards (owner, tailnet): each opens in a new tab at `urlFor(port)`. */
    setPorts(ports, urlFor) {
      $('btn-ports').classList.toggle('hidden', !ports.length);
      if (!ports.length) setMenuOpen('ports', false);
      menus.ports.panel.replaceChildren(
        ...ports.map((port) =>
          menuItem(`localhost:${port}`, 'Open', {
            onSelect: () => {
              setMenuOpen('ports', false);
              window.open(urlFor(port), '_blank', 'noopener');
            },
          }),
        ),
      );
    },
    setAudioAvailable(available) {
      $('btn-audio').classList.toggle('hidden', !available);
    },
    setAudioOn(on) {
      $('audio-waves').style.opacity = on ? '1' : '0.25';
    },
    setMicAvailable(available) {
      $('btn-mic').classList.toggle('hidden', !available);
    },
    setMicOn(on) {
      $('btn-mic').setAttribute('aria-pressed', String(on));
    },
    /** Trackpad mode: offered on touch screens, and whether it's on. */
    setPad({ available, on }) {
      padAvailable = available;
      padActive = on;
      renderMore();
    },
    setTrackpad({ available, on }) {
      if (available !== undefined) $('btn-trackpad').classList.toggle('hidden', !available);
      if (on !== undefined) $('btn-trackpad').setAttribute('aria-pressed', String(on));
    },
    menuItem,
    closeMenus() {
      for (const name of Object.keys(menus)) setMenuOpen(name, false);
    },
    openMenu(name) {
      setToolbarHidden(false);
      for (const other of Object.keys(menus)) setMenuOpen(other, other === name);
      if (name === 'machines') onOpenMachines();
      if (name === 'device') onOpenDevice();
    },
    setDeviceItems(items) {
      menus.device.panel.replaceChildren(...items);
    },
    /** A dot on the device button: something there wants a look (notifications are off). */
    setDeviceHint(on) {
      $('btn-device').classList.toggle('hint', on);
    },
  };
}

/** Names for displays by where they sit: "Left display", "Right display"… */
export function positionNames(displays) {
  const horizontal =
    Math.max(...displays.map((d) => d.x)) - Math.min(...displays.map((d) => d.x)) >=
    Math.max(...displays.map((d) => d.y)) - Math.min(...displays.map((d) => d.y));
  const key = horizontal ? 'x' : 'y';
  const ordered = [...displays].sort((a, b) => a[key] - b[key]);
  const words = horizontal ? ['Left', 'Middle', 'Right'] : ['Top', 'Middle', 'Bottom'];
  const names = new Map();
  ordered.forEach((d, i) => {
    let name;
    if (displays.length === 1) name = d.builtin ? 'Built-in display' : 'Display';
    else if (displays.length === 2) name = `${words[i === 0 ? 0 : 2]} display`;
    else if (displays.length === 3) name = `${words[i]} display`;
    else name = `Display ${i + 1}`;
    names.set(d.id, name);
  });
  return names;
}
