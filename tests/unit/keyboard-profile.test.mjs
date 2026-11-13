import { test } from 'node:test';
import assert from 'node:assert/strict';
import { loadProfile, saveProfile, defaultProfile } from '../../web/keyboard-profile.js';

function store() {
  const m = new Map();
  return { get: (k) => m.get(k) ?? null, set: (k, v) => m.set(k, v) };
}

test('each machine keeps its own remaps', () => {
  const s = store();
  saveProfile(s.set, 'mac.ts.net', { pc: true, ctrlAsCmd: false, swap: true });
  saveProfile(s.set, 'linux.ts.net', { pc: false, ctrlAsCmd: false, swap: false });
  assert.deepEqual(loadProfile(s.get, 'mac.ts.net'), { pc: true, ctrlAsCmd: false, swap: true, physical: false, optionAsAlt: false, saved: true });
  assert.equal(loadProfile(s.get, 'linux.ts.net').pc, false);
});

test('a new Mac starts from the last settings; a new Linux host from its defaults', () => {
  const s = store();
  const fallback = { pc: true, ctrlAsCmd: true, swap: false };
  assert.deepEqual(loadProfile(s.get, 'new-mac', { fallback, hostOs: 'macos' }), { ...fallback, physical: false, optionAsAlt: false, saved: false });
  assert.deepEqual(loadProfile(s.get, 'new-linux', { fallback, hostOs: 'linux', isPC: true }), { pc: false, ctrlAsCmd: false, swap: false, physical: false, optionAsAlt: false, saved: false });
});

test('defaults and junk', () => {
  assert.deepEqual(defaultProfile({ hostOs: 'macos', isPC: true }), { pc: true, ctrlAsCmd: true, swap: false, physical: false, optionAsAlt: false });
  const s = store();
  s.set('glass.keyboard.x', '{not json');
  assert.equal(loadProfile(s.get, 'x').saved, false);
});

test('physical keyboard preference is saved per host and old profiles default to text', () => {
  const s = store();
  saveProfile(s.set, 'gaming', { pc: false, ctrlAsCmd: false, swap: false, physical: true });
  assert.equal(loadProfile(s.get, 'gaming', { hostOs: 'windows' }).physical, true);
  assert.equal(loadProfile(s.get, 'other', { hostOs: 'windows' }).physical, false);
  s.set('glass.keyboard.old', JSON.stringify({ pc: true }));
  assert.equal(loadProfile(s.get, 'old').physical, false);
});

test('Option-as-Alt is independent from physical mode and stays with its host', () => {
  const s = store();
  saveProfile(s.set, 'windows', { physical: false, optionAsAlt: true });
  const p = loadProfile(s.get, 'windows', { hostOs: 'windows' });
  assert.equal(p.optionAsAlt, true);
  assert.equal(p.physical, false);
  assert.equal(loadProfile(s.get, 'other', { hostOs: 'windows' }).optionAsAlt, false);
});
