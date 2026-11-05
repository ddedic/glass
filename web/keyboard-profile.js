// Keyboard remaps per machine: a Linux box and a Mac want different ones (⌘↔Ctrl, PC
// keyboard), so each machine remembers its own. A machine seen for the first time starts
// from the last settings used anywhere, or the defaults for its OS.

export const PROFILE_PREFIX = 'glass.keyboard.';

/** Defaults: PC keyboards map Ctrl to ⌘ on a Mac; on a Linux or Windows host keys go as typed. */
export function defaultProfile({ hostOs = 'macos', isPC = false } = {}) {
  return { pc: hostOs === 'macos' && isPC, ctrlAsCmd: hostOs === 'macos', swap: false, physical: false };
}

/** The profile for `host`: saved for it, else `fallback` (last used), else the defaults. */
export function loadProfile(get, host, { fallback, hostOs, isPC } = {}) {
  const parse = (raw) => {
    try {
      const p = JSON.parse(raw ?? 'null');
      return p && typeof p === 'object' ? p : null;
    } catch {
      return null;
    }
  };
  const base = defaultProfile({ hostOs, isPC });
  const saved = parse(get(PROFILE_PREFIX + host));
  const source = saved ?? (hostOs === 'macos' ? fallback : null) ?? base;
  return {
    pc: Boolean(source.pc ?? base.pc),
    ctrlAsCmd: Boolean(source.ctrlAsCmd ?? base.ctrlAsCmd),
    swap: Boolean(source.swap ?? base.swap),
    physical: Boolean(source.physical ?? base.physical),
    saved: Boolean(saved),
  };
}

export function saveProfile(set, host, { pc, ctrlAsCmd, swap, physical = false }) {
  set(PROFILE_PREFIX + host, JSON.stringify({ pc, ctrlAsCmd, swap, physical }));
}
