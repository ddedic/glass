// Real DOM input handlers, without a native host. Records are observed at the wire boundary.
import { test, expect } from '@playwright/test';
import { readFile } from 'node:fs/promises';

async function openInput(page, options = {}) {
  await page.route('https://input.test/**', async (route) => {
    const name = new URL(route.request().url()).pathname.slice(1);
    if (!name) {
      const body = options.fullUI
        ? (await readFile(new URL('../../web/index.html', import.meta.url), 'utf8')).replace(/<script\b[^>]*>[\s\S]*?<\/script>/g, '')
        : '<div id="screens"><canvas data-stream="0"></canvas></div><textarea id="keys"></textarea><input id="local"><div id="tp-cursor"></div>';
      return route.fulfill({ contentType: 'text/html', body });
    }
    if (!/^[a-z-]+\.js$/.test(name)) return route.fulfill({ body: '' });
    await route.fulfill({ contentType: 'text/javascript', body: await readFile(new URL(`../../web/${name}`, import.meta.url), 'utf8') });
  });
  await page.goto('https://input.test/');
  await page.evaluate(async (options) => {
    const { attachInput } = await import('/input.js');
    window.events = [];
    window.input = attachInput({
      screens: document.querySelector('#screens'), keySink: document.querySelector('#keys'),
      send: (kind, fields) => window.events.push(kind === 12
        ? { text: new TextDecoder().decode(fields.bytes.slice(2, 2 + (fields.bytes[1] & 127))) }
        : { kind, ...fields }),
      control() {}, nextClipboard: () => new Promise(() => {}), macViewer: true, ...options,
    });
    input.setHostOs(options.hostOs ?? 'windows');
  }, options);
}

async function keys(page, sequence) {
  await page.evaluate((sequence) => {
    for (const [type, fields] of sequence) {
      document.querySelector('#keys').dispatchEvent(new KeyboardEvent(type, { bubbles: true, cancelable: true, ...fields }));
    }
  }, sequence);
}

test('German Mac Option+L sends the viewer character without Alt or physical L', async ({ page }) => {
  await openInput(page);
  await keys(page, [
    ['keydown', { code: 'AltLeft', key: 'Alt', altKey: true }],
    ['keydown', { code: 'KeyL', key: '@', altKey: true }],
    ['keyup', { code: 'KeyL', key: '@', altKey: true }],
    ['keyup', { code: 'AltLeft', key: 'Alt' }],
  ]);
  expect(await page.evaluate(() => events)).toEqual([{ text: '@' }]);
});

test('plain hardware typing and repeat send text; Ctrl shortcuts retain positions', async ({ page }) => {
  await openInput(page);
  await page.locator('#keys').focus();
  await page.keyboard.down('a');
  await page.keyboard.down('a');
  await page.keyboard.up('a');
  await page.keyboard.press('Control+s');
  expect(await page.evaluate(() => events)).toEqual([
    { text: 'a' }, { text: 'a' },
    { kind: 5, keycode: 0x3b }, { kind: 5, keycode: 0x01 },
    { kind: 6, keycode: 0x01 }, { kind: 6, keycode: 0x3b },
  ]);
});

test('physical mode retains printable down/up and holds', async ({ page }) => {
  await openInput(page);
  await page.evaluate(() => input.setPhysicalKeyboard(true));
  await page.keyboard.press('a');
  expect(await page.evaluate(() => events)).toEqual([{ kind: 5, keycode: 0 }, { kind: 6, keycode: 0 }]);
});

test('committed ASCII and smart punctuation are verbatim; composition is inserted once', async ({ page }) => {
  await openInput(page);
  await page.locator('#keys').focus();
  await page.keyboard.insertText('@“');
  await page.evaluate(() => {
    const sink = document.querySelector('#keys');
    sink.dispatchEvent(new CompositionEvent('compositionstart'));
    sink.dispatchEvent(new KeyboardEvent('keydown', { code: 'Quote', key: 'Dead', bubbles: true, isComposing: true }));
    sink.dispatchEvent(new CompositionEvent('compositionend', { data: 'é' }));
    sink.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertText', data: 'é', cancelable: true }));
  });
  expect(await page.evaluate(() => events)).toEqual([{ text: '@“' }, { text: 'é' }]);
});

test('AltGr is text on a PC viewer even when Ctrl is remapped on a Mac host', async ({ page }) => {
  await openInput(page, { macViewer: false, hostOs: 'macos', pcKeyboard: true });
  await keys(page, [
    ['keydown', { code: 'ControlLeft', key: 'Control', ctrlKey: true }],
    ['keydown', { code: 'AltRight', key: 'AltGraph', ctrlKey: true, altKey: true, modifierAltGraph: true }],
    ['keydown', { code: 'KeyQ', key: '@', ctrlKey: true, altKey: true, modifierAltGraph: true }],
    ['keyup', { code: 'KeyQ', key: '@', ctrlKey: true, altKey: true, modifierAltGraph: true }],
    ['keyup', { code: 'AltRight', key: 'AltGraph', ctrlKey: true }],
    ['keyup', { code: 'ControlLeft', key: 'Control' }],
  ]);
  expect(await page.evaluate(() => events)).toEqual([{ text: '@' }]);
});

test('Ctrl+Alt is a physical shortcut without an explicit AltGraph state', async ({ page }) => {
  await openInput(page, { macViewer: false });
  await page.keyboard.press('Control+Alt+q');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 0x3b }, { kind: 5, keycode: 0x3a }, { kind: 5, keycode: 0x0c },
    { kind: 6, keycode: 0x0c }, { kind: 6, keycode: 0x3a }, { kind: 6, keycode: 0x3b },
  ]);
});

test('Shift typing, pointer chords, blur and a mode change leave no keys held', async ({ page }) => {
  await openInput(page);
  await page.keyboard.down('Shift');
  await page.keyboard.press('A');
  await page.locator('canvas').click({ position: { x: 10, y: 10 } });
  await page.evaluate(() => window.dispatchEvent(new Event('blur')));
  await page.keyboard.up('Shift');
  await page.evaluate(() => input.setPhysicalKeyboard(true));
  await page.keyboard.down('w');
  await page.evaluate(() => input.setPhysicalKeyboard(false));
  await page.keyboard.up('w');
  const events = await page.evaluate(() => window.events.filter(e => e.text || [2, 3, 5, 6].includes(e.kind)));
  expect(events).toEqual([
    { text: 'A' }, { kind: 5, keycode: 0x38 }, { kind: 2, button: 0 },
    { kind: 3, button: 0 }, { kind: 6, keycode: 0x38 },
    { kind: 5, keycode: 0x0d }, { kind: 6, keycode: 0x0d },
  ]);
});

test('dead-key composition cancels without input and local fields stay local', async ({ page }) => {
  await openInput(page);
  await page.locator('#keys').focus();
  await keys(page, [['keydown', { code: 'Quote', key: 'Dead' }], ['keyup', { code: 'Quote', key: 'Dead' }]]);
  await page.evaluate(() => {
    const sink = document.querySelector('#keys');
    sink.dispatchEvent(new CompositionEvent('compositionstart'));
    sink.dispatchEvent(new CompositionEvent('compositionend', { data: '' }));
  });
  await page.locator('#local').fill('local');
  await page.keyboard.press('a');
  expect(await page.evaluate(() => events)).toEqual([]);
});

test('Linux keeps physical input; Mac Ctrl-as-Command holds the remap across Tab repeats', async ({ page }) => {
  await openInput(page, { hostOs: 'linux' });
  await page.keyboard.press('a');
  expect(await page.evaluate(() => events)).toEqual([{ kind: 5, keycode: 0 }, { kind: 6, keycode: 0 }]);
  await page.evaluate(() => { input.setHostOs('macos'); events.length = 0; });
  await page.keyboard.down('Control');
  await page.keyboard.press('Tab');
  await page.keyboard.press('Tab');
  await page.keyboard.up('Control');
  expect(await page.evaluate(() => events)).toEqual([
    { kind: 5, keycode: 0x3b }, { kind: 6, keycode: 0x3b }, { kind: 5, keycode: 0x37 },
    { kind: 5, keycode: 0x30 }, { kind: 6, keycode: 0x30 },
    { kind: 5, keycode: 0x30 }, { kind: 6, keycode: 0x30 }, { kind: 6, keycode: 0x37 },
  ]);
});

test('desktop touchscreen computers focus the composition sink on mouse click', async ({ page }) => {
  await page.addInitScript(() => Object.defineProperty(navigator, 'maxTouchPoints', { value: 10 }));
  await openInput(page, { macViewer: false });
  await page.locator('canvas').click({ position: { x: 10, y: 10 } });
  await expect(page.locator('#keys')).toBeFocused();
});

test('the physical keyboard menu changes routing and is hidden for Linux', async ({ page }) => {
  await openInput(page, { fullUI: true });
  await page.evaluate(async () => {
    const { createUI } = await import('/ui.js');
    window.ui = createUI({ shortcuts: [], onPhysicalKeyboard: on => input.setPhysicalKeyboard(on) });
    ui.setHostOs('windows');
    ui.setKeyboardProfile({ physical: false });
  });
  await page.locator('#btn-more').click();
  const item = page.locator('#more-panel').getByRole('menuitemradio', { name: /Physical keyboard/ });
  await expect(item).toHaveAttribute('aria-checked', 'false');
  await item.click();
  await expect(item).toHaveAttribute('aria-checked', 'true');
  await page.locator('#keys').focus();
  await page.keyboard.press('a');
  expect(await page.evaluate(() => events)).toEqual([{ kind: 5, keycode: 0 }, { kind: 6, keycode: 0 }]);
  await page.evaluate(() => ui.setHostOs('linux'));
  await expect(item).toHaveCount(0);
});

test('modifier changes during a drag reach the host before movement', async ({ page }) => {
  await openInput(page);
  await page.evaluate(() => input.setVideoSize(0, 300, 150));
  await page.mouse.move(10, 10);
  await page.mouse.down();
  await page.keyboard.down('Alt');
  await page.mouse.move(20, 20);
  await page.keyboard.up('Alt');
  await page.mouse.up();
  const events = await page.evaluate(() => window.events);
  const alt = events.findIndex(e => e.kind === 5 && e.keycode === 0x3a);
  const move = events.findIndex((e, i) => i > events.findIndex(e => e.kind === 2) && e.kind === 1);
  expect(alt).toBeGreaterThan(-1);
  expect(alt).toBeLessThan(move);
});

test('Command release clears text keys whose keyup macOS swallowed', async ({ page }) => {
  await openInput(page, { hostOs: 'macos' });
  await keys(page, [
    ['keydown', { code: 'KeyA', key: 'a' }],
    ['keydown', { code: 'MetaLeft', key: 'Meta', metaKey: true }],
    ['keyup', { code: 'MetaLeft', key: 'Meta' }],
    ['keydown', { code: 'MetaLeft', key: 'Meta', metaKey: true }],
    ['keydown', { code: 'KeyA', key: 'a', metaKey: true }],
    ['keyup', { code: 'KeyA', key: 'a', metaKey: true }],
    ['keyup', { code: 'MetaLeft', key: 'Meta' }],
  ]);
  expect(await page.evaluate(() => events.filter(e => e.keycode === 0))).toEqual([
    { kind: 5, keycode: 0 }, { kind: 6, keycode: 0 },
  ]);
});
