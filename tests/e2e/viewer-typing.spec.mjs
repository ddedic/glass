// Exercise the real DOM handlers and observe the existing input wire boundary.
import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';

async function open(page, { os = 'windows', capable = true, mac = true } = {}) {
  await page.route('https://input.test/**', async route => {
    const name = new URL(route.request().url()).pathname.slice(1);
    if (name && !/^[a-z-]+\.js$/.test(name)) return route.fulfill({ body: '' });
    await route.fulfill(name ? {
      contentType: 'text/javascript', body: await readFile(new URL(`../../web/${name}`, import.meta.url), 'utf8'),
    } : { contentType: 'text/html', body: '<div id="screens"></div><textarea id="keys"></textarea><div id="tp-cursor"></div>' });
  });
  await page.goto('https://input.test/');
  await page.evaluate(async ({ os, capable, mac }) => {
    const { attachInput } = await import('/input.js');
    window.events = [];
    window.input = attachInput({
      screens: document.querySelector('#screens'), keySink: document.querySelector('#keys'), macViewer: mac,
      send: (kind, fields) => events.push(kind === 12
        ? { text: new TextDecoder().decode(fields.bytes.slice(2, 2 + (fields.bytes[1] & 127))) }
        : { kind, ...fields }),
      control() {}, nextClipboard: () => new Promise(() => {}),
    });
    input.setHostOs(os);
    input.setViewerTyping?.(capable);
  }, { os, capable, mac });
}
async function key(page, type, code, value, extra = {}) {
  await page.evaluate(({ type, code, value, extra }) => window.dispatchEvent(new KeyboardEvent(type, {
    code, key: value, bubbles: true, cancelable: true, ...extra,
  })), { type, code, value, extra });
}
async function press(page, code, value, extra = {}) {
  await key(page, 'keydown', code, value, extra);
  await key(page, 'keyup', code, value, extra);
}

test('German viewer characters and repeats reach Windows as text, not physical Y/Z', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'KeyY', 'z');
  await key(page, 'keydown', 'KeyY', 'z', { repeat: true });
  await key(page, 'keyup', 'KeyY', 'z');
  await press(page, 'BracketLeft', 'ü');
  expect(await page.evaluate(() => events)).toEqual([{ text: 'z' }, { text: 'z' }, { text: 'ü' }]);
});

for (const options of [{ os: 'macos' }, { os: 'linux' }, { capable: false }, { mac: false }]) {
  test(`physical fallback ${JSON.stringify(options)}`, async ({ page }) => {
    await open(page, options);
    await press(page, 'KeyY', 'z');
    expect(await page.evaluate(() => events)).toEqual([{ kind: 5, keycode: 16 }, { kind: 6, keycode: 16 }]);
  });
}

test('Space remains held through modifier changes; shortcuts stay physical', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'Space', ' ');
  await key(page, 'keydown', 'ShiftLeft', 'Shift', { shiftKey: true });
  await key(page, 'keyup', 'ShiftLeft', 'Shift');
  await key(page, 'keyup', 'Space', ' ');
  await key(page, 'keydown', 'AltLeft', 'Alt', { altKey: true });
  await press(page, 'KeyD', '∂', { altKey: true });
  await key(page, 'keyup', 'AltLeft', 'Alt');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 49 }, { kind: 5, keycode: 56 }, { kind: 6, keycode: 56 }, { kind: 6, keycode: 49 },
    { kind: 5, keycode: 58 }, { kind: 5, keycode: 2 }, { kind: 6, keycode: 2 }, { kind: 6, keycode: 58 },
  ]);
});

test('Shift types the viewer character and a later Ctrl does not change the press route', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'ShiftLeft', 'Shift', { shiftKey: true });
  await key(page, 'keydown', 'Digit2', '"', { shiftKey: true });
  await key(page, 'keyup', 'ShiftLeft', 'Shift');
  await key(page, 'keyup', 'Digit2', '2');
  await key(page, 'keydown', 'KeyY', 'z');
  await key(page, 'keydown', 'ControlLeft', 'Control', { ctrlKey: true });
  await key(page, 'keyup', 'KeyY', 'z', { ctrlKey: true });
  await key(page, 'keyup', 'ControlLeft', 'Control');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 56 }, { text: '"' }, { kind: 6, keycode: 56 }, { text: 'z' },
    { kind: 5, keycode: 59 }, { kind: 6, keycode: 59 },
  ]);
});

test('committed ASCII and dead-key text bypass US key conversion only for capable Windows', async ({ page }) => {
  await open(page);
  await page.locator('#keys').focus();
  await page.keyboard.insertText('z@');
  await page.evaluate(() => document.querySelector('#keys').dispatchEvent(new CompositionEvent('compositionend', { data: 'č' })));
  expect(await page.evaluate(() => events)).toEqual([{ text: 'z@' }, { text: 'č' }]);
});

test('a physical shortcut repeat cannot turn into text after releasing Ctrl', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'ControlLeft', 'Control', { ctrlKey: true });
  await key(page, 'keydown', 'KeyY', 'z', { ctrlKey: true });
  await key(page, 'keyup', 'ControlLeft', 'Control');
  await key(page, 'keydown', 'KeyY', 'z', { repeat: true });
  await key(page, 'keyup', 'KeyY', 'z');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 59 }, { kind: 5, keycode: 16 }, { kind: 6, keycode: 59 },
    { kind: 5, keycode: 16 }, { kind: 6, keycode: 16 },
  ]);
});

test('Dead starts local composition without a host key; reconnect clears the capability', async ({ page }) => {
  await open(page);
  await press(page, 'Equal', 'Dead');
  expect(await page.evaluate(() => ({ events, focus: document.activeElement.id }))).toEqual({ events: [], focus: 'keys' });
  await page.evaluate(() => input.setHostOs('windows'));
  await press(page, 'KeyY', 'z');
  expect(await page.evaluate(() => events)).toEqual([{ kind: 5, keycode: 16 }, { kind: 6, keycode: 16 }]);
});

test('Option dead-key chords stay physical instead of starting local composition', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'AltLeft', 'Alt', { altKey: true });
  await press(page, 'KeyE', 'Dead', { altKey: true });
  await key(page, 'keyup', 'AltLeft', 'Alt');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 58 }, { kind: 5, keycode: 14 }, { kind: 6, keycode: 14 }, { kind: 6, keycode: 58 },
  ]);
});

test('Cmd release retires a text press whose keyup was swallowed before the next shortcut', async ({ page }) => {
  await open(page);
  await key(page, 'keydown', 'KeyY', 'z');
  await key(page, 'keydown', 'MetaLeft', 'Meta', { metaKey: true });
  // The browser swallowed Y-up while Cmd was held.
  await key(page, 'keyup', 'MetaLeft', 'Meta');
  await page.evaluate(() => { events.length = 0; });
  await key(page, 'keydown', 'ControlLeft', 'Control', { ctrlKey: true });
  await press(page, 'KeyY', 'z', { ctrlKey: true });
  await key(page, 'keyup', 'ControlLeft', 'Control');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 59 }, { kind: 5, keycode: 16 }, { kind: 6, keycode: 16 }, { kind: 6, keycode: 59 },
  ]);
});
