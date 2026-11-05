// End-to-end tests against real `glass --test` hosts in Google Chrome.
// The host streams a test pattern on two fake 640×360 displays and records input.

import { execFileSync } from 'node:child_process';
import { BINARY, Host, expectVideo, hudText, pickMenuItem, stats, touch, test, expect, toolbarClick } from './harness.mjs';

/** One host per test, stopped afterwards. */
const it = test.extend({
  host: async ({}, use) => {
    const host = await new Host().start();
    await use(host);
    await host.stop();
  },
});

/** Mac virtual keycodes the assertions refer to. */
const K = { A: 0x00, S: 0x01, H: 0x04, I: 0x22, V: 0x09, C: 0x08, Tab: 0x30, Shift: 0x38, Cmd: 0x37, Ctrl: 0x3b };

async function open(page, host, options) {
  await page.goto(host.url(options));
  await expectVideo(page);
}

/** Raw sign-in over WebTransport; returns the host's first control message as text. */
function signIn(page, host, token) {
  return page.evaluate(
    async ({ wt, token }) => {
      const config = await fetch('/config.json').then((r) => r.json());
      const t = new WebTransport(`https://localhost:${wt}/`, {
        serverCertificateHashes: [{ algorithm: 'sha-256', value: new Uint8Array(config.certHash) }],
      });
      await t.ready;
      const { readable, writable } = await t.createBidirectionalStream();
      const body = new TextEncoder().encode(JSON.stringify({ t: 'hello', token }));
      const frame = new Uint8Array(4 + body.length);
      new DataView(frame.buffer).setUint32(0, body.length, true);
      frame.set(body, 4);
      await writable.getWriter().write(frame);
      const { value } = await readable.getReader().read();
      t.close();
      return new TextDecoder().decode(value.slice(4));
    },
    { wt: host.wt, token },
  );
}

async function keys(host, type) {
  return (await host.input(type)).events.map((e) => e.keycode);
}

test.describe('connecting', () => {
  it('locks out an address after repeated wrong tokens', async ({ page, host }) => {
    const attempt = (token) => signIn(page, host, token);
    await page.goto(`${host.base}/config.json`);
    expect(await attempt(host.token)).toContain('welcome');
    for (let i = 0; i < 10; i++) expect(await attempt('wrong')).toContain('unauthorized');
    // Locked out now, even with the right token.
    expect(await attempt(host.token)).toContain('unauthorized');
  });

  it('skips control messages it does not know instead of hanging up', async ({ page, host }) => {
    await page.goto(`${host.base}/config.json`);
    const reply = await page.evaluate(
      async ({ wt, token }) => {
        const config = await fetch('/config.json').then((r) => r.json());
        const t = new WebTransport(`https://localhost:${wt}/`, {
          serverCertificateHashes: [{ algorithm: 'sha-256', value: new Uint8Array(config.certHash) }],
        });
        await t.ready;
        const { readable, writable } = await t.createBidirectionalStream();
        const writer = writable.getWriter();
        const send = (msg) => {
          const body = new TextEncoder().encode(JSON.stringify(msg));
          const frame = new Uint8Array(4 + body.length);
          new DataView(frame.buffer).setUint32(0, body.length, true);
          frame.set(body, 4);
          return writer.write(frame);
        };
        await send({ t: 'hello', token });
        // A message from a newer client, then a display switch that must still work.
        await send({ t: 'somethingFromTheFuture', x: 1 });
        await send({ t: 'showAll' });
        const reader = readable.getReader();
        let text = '';
        const deadline = Date.now() + 5000;
        while (!text.includes('"layout"') && Date.now() < deadline) {
          const { value, done } = await reader.read();
          if (done) break;
          text += new TextDecoder().decode(value);
        }
        t.close();
        return text;
      },
      { wt: host.wt, token: host.token },
    );
    expect(reply).toContain('"layout"');
  });

  it('glass token --reset takes effect without a restart', async ({ page, host }) => {
    await page.goto(`${host.base}/config.json`);
    const old = host.token;
    execFileSync(BINARY, ['token', '--reset'], { env: { ...process.env, GLASS_CONFIG_DIR: host.configDir }, stdio: 'pipe' });
    expect(host.token).not.toBe(old);
    expect(await signIn(page, host, old)).toContain('unauthorized');
    expect(await signIn(page, host, host.token)).toContain('welcome');
  });

  it('decodes video with low latency', async ({ page, host }) => {
    await open(page, host);
    await expect.poll(async () => (await stats(page)).fps, { timeout: 10_000 }).toBeGreaterThan(15);
    // Judge a few seconds of one-second samples: one busy second on a loaded test machine
    // shouldn't fail the test, a consistently slow pipeline should.
    const samples = [];
    for (let i = 0; i < 4; i++) {
      await page.waitForTimeout(1000);
      samples.push(await stats(page));
    }
    const latencies = samples.map((s) => s.latency).sort((a, b) => a - b);
    expect(latencies[0]).toBeGreaterThan(0);
    expect(latencies[1]).toBeLessThan(100);
    expect(samples.every((s) => s.dropped === 0)).toBeTruthy();
  });

  it('removes the token from the URL and remembers it', async ({ page, host }) => {
    await open(page, host);
    expect(page.url()).not.toContain('#t=');
    await page.reload();
    await expectVideo(page);
  });

  it('asks for a token, rejects a wrong one, accepts the right one', async ({ page, host }) => {
    await page.goto(host.url({ token: false }));
    const auth = page.locator('#auth');
    await expect(auth).toBeVisible();
    await expect(page.locator('#screens canvas').first()).toBeAttached();
    expect(await hudText(page)).not.toContain('640×360');

    // The token form (the directory sign-in fields beside it are hidden unless set up).
    await auth.locator('input[name="token"]').fill('not-the-token');
    await auth.locator('button[type="submit"]').click();
    await expect(auth).toBeVisible({ timeout: 10_000 });

    await auth.locator('input[name="token"]').fill(host.token);
    await auth.locator('button[type="submit"]').click();
    await expectVideo(page);
    await expect(auth).toBeHidden();
  });

  it('sends nothing to an unauthenticated session', async ({ page, host }) => {
    await page.goto(`${host.base}/config.json`); // a same-origin page that doesn't run the client
    const result = await page.evaluate(async (wt) => {
      const config = await fetch('/config.json').then((r) => r.json());
      const t = new WebTransport(`https://localhost:${wt}/`, {
        serverCertificateHashes: [{ algorithm: 'sha-256', value: new Uint8Array(config.certHash) }],
      });
      await t.ready;
      // Input before hello must be ignored.
      const input = await t.createUnidirectionalStream();
      const w = input.getWriter();
      await w.write(new Uint8Array([0x49, 5, 0, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
      const reader = t.datagrams.readable.getReader();
      const got = await Promise.race([
        reader.read().then(() => 'datagram'),
        new Promise((r) => setTimeout(() => r('none'), 1500)),
      ]);
      t.close();
      return got;
    }, host.wt);
    expect(result).toBe('none');
    expect((await host.input()).events).toHaveLength(0);
  });

  it('serves two clients at once', async ({ browser, host }) => {
    const [a, b] = [await browser.newPage(), await browser.newPage()];
    await open(a, host);
    await open(b, host);
    await expect.poll(async () => (await stats(a)).fps).toBeGreaterThan(10);
    await expect.poll(async () => (await stats(b)).fps).toBeGreaterThan(10);
  });
});

test.describe('magic links', () => {
  it('a link from the host lets one device in (in more than one browser) until revoked', async ({ page, browser, host }) => {
    await open(page, host);
    await page.waitForTimeout(1500); // the host has written its status, which `glass link` reads
    const env = { ...process.env, GLASS_CONFIG_DIR: host.configDir };
    const made = JSON.parse(execFileSync(BINARY, ['link', '--json', '--for', '1h'], { env }).toString());
    expect(made.summary).toBe('Works on one device, within 15 minutes. Gives that device 1 hour of access (full control).');
    const code = made.url.match(/#l=([0-9a-f]{32})$/)[1];
    const link = `${host.base}/#l=${code}`;

    // The guest: a separate browser with no token.
    const guestContext = await browser.newContext();
    const guest = await guestContext.newPage();
    await guest.goto(link);
    await expectVideo(guest);
    await expect(guest.locator('.toast', { hasText: "You're in" })).toBeVisible();
    expect(guest.url()).not.toContain('#l=');
    await expect(guest.locator('#btn-machines')).toBeHidden();

    // The pass survives a reload.
    await guest.reload();
    await expectVideo(guest);
    // A different kind of device finds the link spent...
    const windows = await browser.newContext({ userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36' });
    const other = await windows.newPage();
    await other.goto(link);
    await expect(other.locator('.toast', { hasText: 'expired or was already used' })).toBeVisible();
    await expect(other.locator('#auth')).toBeVisible();
    // ...but the same device (same network and system) can open it again in another browser.
    const againContext = await browser.newContext();
    const again = await againContext.newPage();
    await again.goto(link);
    await expectVideo(again);
    await expect(again.locator('.toast', { hasText: "You're in" })).toBeVisible();

    // Revoking cuts both browsers off; on reconnect the first is asked for a token.
    expect(JSON.parse(execFileSync(BINARY, ['link', '--revoke', '--json'], { env }).toString())).toEqual({ revoked: 2 });
    await expect(guest.locator('#auth')).toBeVisible({ timeout: 20_000 });
    // The owner is unaffected.
    await expect.poll(async () => (await stats(page)).fps).toBeGreaterThan(0);
    await againContext.close();
    await windows.close();
    await guestContext.close();
  });

  it('glass link prints a working link for the running host', async ({ browser, host }) => {
    const page = await (await browser.newContext()).newPage();
    await page.goto(`${host.base}/config.json`); // host is up and has written its status
    await page.waitForTimeout(1500);
    const out = execFileSync(BINARY, ['link', '--for', '2h'], { env: { ...process.env, GLASS_CONFIG_DIR: host.configDir } }).toString();
    expect(out).toContain('Gives that device 2 hours of access');
    const code = out.match(/#l=([0-9a-f]{32})/)[1];
    await page.goto(`${host.base}/#l=${code}`);
    await expectVideo(page);
  });
});

test.describe('ports', () => {
  it('moves to the next free ports when the usual ones are taken', async ({ page, host }) => {
    // A second host asked for the same ports as the running one.
    const second = new Host();
    Object.assign(second, { http: host.http, wt: host.wt, base: host.base });
    await second.start(); // returns once *a* host answers there; find where this one went
    try {
      const statusFile = `${second.configDir}/status.json`;
      const { readFileSync, existsSync } = await import('node:fs');
      await expect.poll(() => existsSync(statusFile) && JSON.parse(readFileSync(statusFile, 'utf8')).url, { timeout: 15_000 })
        .toMatch(/^http:\/\/localhost:\d+\/$/);
      const url = JSON.parse(readFileSync(statusFile, 'utf8')).url;
      const port = Number(new URL(url).port);
      expect(port).toBeGreaterThan(host.http);
      const config = await (await fetch(`${url}config.json`)).json();
      expect(config.wtPort).toBeGreaterThan(host.wt);
      // And it works there.
      const token = readFileSync(`${second.configDir}/token`, 'utf8').trim();
      await page.goto(`${url}#t=${token}`);
      await expectVideo(page);
    } finally {
      await second.stop();
    }
  });
});

test.describe('displays', () => {
  it('offers sizes in this device\'s shape, and puts the displays back after', async ({ page, host }) => {
    // A square window against a 16:9 display: "fit this device" offers square sizes.
    await page.setViewportSize({ width: 1100, height: 1100 });
    await open(page, host);
    await page.locator('#btn-displays').dispatchEvent('click');
    const fit = page.locator('.panel:not(.hidden) button', { hasText: '1100×1100' });
    await expect(fit).toHaveCount(1);
    await fit.dispatchEvent('click');
    await expect.poll(async () => (await host.get('/test/fit')).requests).toEqual([[1100, 1100]]);
    // Back to Auto restores the displays.
    await page.locator('#btn-displays').dispatchEvent('click');
    await page.locator('.panel:not(.hidden) button', { hasText: 'Auto' }).dispatchEvent('click');
    await expect.poll(async () => (await host.get('/test/fit')).requests).toEqual([[1100, 1100], null]);
  });

  it('encodes at the size this screen shows, or the full size on request', async ({ page, host }) => {
    // A small window: the 640×360 display only needs ~300 px, so the host sends half
    // size (the test encoder, like Linux's, scales by halves; Macs scale to any width).
    // Not open(): on a slow machine the first frame may already be the half-size one.
    await page.setViewportSize({ width: 300, height: 240 });
    await page.goto(host.url());
    await expect.poll(() => hudText(page), { timeout: 15_000 }).toContain('320×180');
    // "This Mac" asks for full resolution again.
    await page.locator('#btn-displays').dispatchEvent('click');
    await page.locator('.panel:not(.hidden) button', { hasText: 'This Mac' }).dispatchEvent('click');
    await expect.poll(() => hudText(page), { timeout: 15_000 }).toContain('640×360');
    expect(await page.evaluate(() => localStorage.getItem('glass.resolution'))).toBe('native');
  });

  it('lists displays and switches between one and all', async ({ page, host }) => {
    await open(page, host);
    await toolbarClick(page, 'btn-displays');
    const items = page.locator('#displays-panel button');
    // Two displays, "All displays", "Use as a second display", the resolution choices
    // (Auto, This Mac), Sharp text, then the test host's two windows (Apps).
    await expect(items).toHaveCount(9);
    await expect(items.nth(2)).toContainText('All displays');
    await expect(items.nth(3)).toContainText('Use as a second display');
    await expect(items.nth(4)).toContainText('Auto');
    await expect(items.nth(5)).toContainText('This Mac');
    await items.nth(2).click();
    await expectVideo(page, 2);
    await expect(page.locator('#screens canvas:not(.hidden)')).toHaveCount(2);

    await pickMenuItem(page, 'btn-displays', 'Right display');
    await expectVideo(page, 1);
    await expect(page.locator('#screens canvas:not(.hidden)')).toHaveCount(1);
  });
});

test.describe('input', () => {
  it('maps mouse position and clicks onto the right display', async ({ page, host }) => {
    await open(page, host);
    await pickMenuItem(page, 'btn-displays', 'All displays');
    await expectVideo(page, 2);
    await host.clearInput();

    const canvases = page.locator('#screens canvas:not(.hidden)');
    for (const [i, canvas] of [canvases.nth(0), canvases.nth(1)].entries()) {
      const box = await canvas.boundingBox();
      // Letterboxed: the 16:9 video sits centred inside the canvas box.
      await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
      await page.mouse.down();
      await page.mouse.up();
      await expect.poll(async () => (await host.input('move')).events.filter((e) => e.stream === i).length).toBeGreaterThan(0);
      const move = (await host.input('move')).events.filter((e) => e.stream === i).at(-1);
      expect(move.x).toBeCloseTo(0.5, 1);
      expect(move.y).toBeCloseTo(0.5, 1);
    }
    const { events } = await host.input();
    expect(events.filter((e) => e.t === 'down' && e.button === 0)).toHaveLength(2);
    expect(events.filter((e) => e.t === 'up' && e.button === 0)).toHaveLength(2);
  });

  it('sends right clicks and wheel scrolling', async ({ page, host }) => {
    await open(page, host);
    const box = await page.locator('#screens canvas').first().boundingBox();
    await page.mouse.move(box.x + 100, box.y + 100);
    await host.clearInput();
    await page.mouse.click(box.x + 100, box.y + 100, { button: 'right' });
    await page.mouse.wheel(0, 120);
    await expect.poll(async () => (await host.input('scroll')).events.length).toBeGreaterThan(0);
    const { events } = await host.input();
    expect(events.some((e) => e.t === 'down' && e.button === 2)).toBeTruthy();
    // In the host's desktop points: the 640-point display is shown at another size.
    const shown = Math.min(box.width / 640, box.height / 360) * 640;
    expect((await host.input('scroll')).events.reduce((sum, e) => sum + e.dy, 0)).toBeCloseTo(120 * 640 / shown, 0);
  });

  it('physical keyboard mode sends typed keys as Mac keycodes', async ({ page, host }) => {
    await open(page, host);
    await pickMenuItem(page, 'btn-more', 'Physical keyboard');
    await host.clearInput();
    // Like a real keyboard: Shift is its own key press.
    await page.keyboard.press('Shift+KeyH');
    await page.keyboard.press('KeyI');
    await expect.poll(() => keys(host, 'keyup')).toEqual([K.H, K.Shift, K.I]);
    expect(await keys(host, 'keydown')).toEqual([K.Shift, K.H, K.I]);
  });

  it('types IME, emoji and accented text as text, in order with keys', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await page.locator('#keys').focus();
    await page.keyboard.insertText('café 👍');
    // An IME settling on what was composed.
    await page.evaluate(() => {
      const sink = document.getElementById('keys');
      sink.dispatchEvent(new CompositionEvent('compositionstart', { data: '' }));
      sink.dispatchEvent(new CompositionEvent('compositionend', { data: '日本語' }));
    });
    // ASCII uses the same text path; editing keys remain physical.
    await page.keyboard.insertText('i');
    await page.keyboard.press('Tab');
    await expect.poll(() => keys(host, 'keyup')).toEqual([K.Tab]);
    const { events } = await host.input();
    const typed = events.filter((e) => e.t === 'text' || e.t === 'keydown').map((e) => e.text ?? e.keycode);
    expect(typed).toEqual(['café 👍', '日本語', 'i', K.Tab]);
  });

  it('PC keyboard mode turns Ctrl into ⌘ and Super into Ctrl', async ({ page, host }) => {
    await page.goto(host.url());
    await page.evaluate(() => localStorage.setItem('glass.pcKeyboard', '1'));
    await page.reload();
    await expectVideo(page);
    await host.clearInput();
    await page.keyboard.press('Control+S');
    await page.keyboard.press('Meta');
    await expect.poll(() => keys(host, 'keydown')).toEqual([K.Cmd, K.S, K.Ctrl]);
  });

  it('Ctrl as ⌘ turns Ctrl+Tab into ⌘Tab and keeps other Ctrl combos', async ({ page, host }) => {
    // Mac keyboard conventions (Linux/Windows browsers default to PC keyboard mode).
    await page.goto(host.url());
    await page.evaluate(() => localStorage.setItem('glass.pcKeyboard', '0'));
    await page.reload();
    await expectVideo(page);
    await host.clearInput();
    await page.keyboard.down('Control');
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    await page.keyboard.up('Control');
    await page.keyboard.press('Control+A');
    await expect.poll(() => keys(host, 'keydown')).toEqual([K.Ctrl, K.Cmd, K.Tab, K.Tab, K.Ctrl, K.A]);
    // The ⌘ stand-in is released with Ctrl.
    expect((await keys(host, 'keyup')).filter((k) => k === K.Cmd)).toHaveLength(1);
  });

  test('a Linux host gets keys as typed and no Mac shortcuts menu', async ({ page }) => {
    const host = await new Host({ os: 'linux' }).start();
    try {
      await page.goto(host.url());
      await page.evaluate(() => localStorage.setItem('glass.pcKeyboard', '1'));
      await page.reload();
      await expectVideo(page);
      await expect(page.locator('#btn-shortcuts')).toBeHidden();
      await host.clearInput();
      // PC keyboard mode and Ctrl as ⌘ only make sense for Mac hosts.
      await page.keyboard.press('Control+S');
      await page.keyboard.down('Control');
      await page.keyboard.press('Tab');
      await page.keyboard.up('Control');
      await expect.poll(() => keys(host, 'keydown')).toEqual([K.Ctrl, K.S, K.Ctrl, K.Tab]);
    } finally {
      await host.stop();
    }
  });

  it('stays put until hidden, comes back from its tab, and remembers', async ({ page, host }) => {
    await open(page, host);
    const toolbar = page.locator('#toolbar');
    const tab = page.locator('#toolbar-show');
    // No hiding by itself.
    await page.waitForTimeout(4000);
    await expect(toolbar).not.toHaveClass(/tucked/);
    await expect(tab).toBeHidden();

    await page.locator('#btn-hide-toolbar').click();
    await expect(toolbar).toHaveClass(/tucked/);
    await expect(tab).toBeVisible();
    // Its buttons are out of reach while hidden.
    expect(await page.evaluate(() => document.getElementById('toolbar').inert)).toBe(true);
    await page.reload();
    await expectVideo(page);
    await expect(toolbar).toHaveClass(/tucked/);

    await host.clearInput();
    await tab.click();
    await expect(toolbar).not.toHaveClass(/tucked/);
    await expect(tab).toBeHidden();
    // The tap went to the tab, not the remote screen.
    expect((await host.input()).events.filter((e) => e.t === 'down')).toHaveLength(0);
    expect(await page.evaluate(() => localStorage.getItem('glass.toolbarHidden'))).toBe('0');
  });
  it('touch: tap clicks, long press right-clicks, two fingers scroll', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await touch(page, 0, [{ type: 'pointerdown', fx: 0.25, fy: 0.25 }, { type: 'pointerup', fx: 0.25, fy: 0.25 }]);
    await expect.poll(async () => (await host.input('up')).events.length).toBe(1);
    expect((await host.input('down')).events[0].button).toBe(0);

    await host.clearInput();
    await touch(page, 0, [{ type: 'pointerdown', fx: 0.5, fy: 0.5 }]);
    await page.waitForTimeout(700);
    await touch(page, 0, [{ type: 'pointerup', fx: 0.5, fy: 0.5 }]);
    await expect.poll(async () => (await host.input('down')).events.map((e) => e.button)).toContain(2);

    await host.clearInput();
    // Two fingers moving up together, in small steps like a real swipe.
    const steps = Array.from({ length: 10 }, (_, i) => 0.6 - (i + 1) * 0.02).flatMap((fy) => [
      { type: 'pointermove', id: 1, fx: 0.4, fy },
      { type: 'pointermove', id: 2, fx: 0.6, fy },
    ]);
    await touch(page, 0, [
      { type: 'pointerdown', id: 1, fx: 0.4, fy: 0.6 },
      { type: 'pointerdown', id: 2, fx: 0.6, fy: 0.6 },
      ...steps,
      { type: 'pointerup', id: 1, fx: 0.4, fy: 0.4 },
      { type: 'pointerup', id: 2, fx: 0.6, fy: 0.4 },
    ]);
    await expect.poll(async () => (await host.input('scroll')).events.length).toBeGreaterThan(0);
    // Fingers moved up, so content scrolls down (positive dy), and nothing was clicked.
    expect((await host.input('scroll')).events.reduce((s, e) => s + e.dy, 0)).toBeGreaterThan(0);
    expect((await host.input('down')).events).toHaveLength(0);
  });

  it('pinch zooms locally, taps still land in the right place, reset restores', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    // Pinch out around the centre.
    await touch(page, 0, [
      { type: 'pointerdown', id: 1, fx: 0.45, fy: 0.5 },
      { type: 'pointerdown', id: 2, fx: 0.55, fy: 0.5 },
      { type: 'pointermove', id: 1, fx: 0.4, fy: 0.5 },
      { type: 'pointermove', id: 2, fx: 0.6, fy: 0.5 },
      { type: 'pointermove', id: 1, fx: 0.35, fy: 0.5 },
      { type: 'pointermove', id: 2, fx: 0.65, fy: 0.5 },
      { type: 'pointerup', id: 1, fx: 0.35, fy: 0.5 },
      { type: 'pointerup', id: 2, fx: 0.65, fy: 0.5 },
    ]);
    await expect(page.locator('#zoom-pill')).toBeVisible();
    // Fingers went from 10% to 30% of the width apart: 3×.
    await expect(page.locator('#zoom-level')).toHaveText('3.0×');
    // Zooming is local: nothing reached the Mac.
    expect((await host.input()).events.filter((e) => e.t !== 'move')).toHaveLength(0);

    // The centre of the (zoomed) canvas is still the centre of the remote screen.
    await host.clearInput();
    const view = page.viewportSize();
    await page.evaluate(({ x, y }) => {
      const canvas = document.querySelector('#screens canvas');
      for (const type of ['pointerdown', 'pointerup']) {
        canvas.dispatchEvent(new PointerEvent(type, { bubbles: true, cancelable: true, pointerType: 'touch', pointerId: 3, clientX: x, clientY: y }));
      }
    }, { x: view.width / 2, y: view.height / 2 });
    await expect.poll(async () => (await host.input('move')).events.length).toBeGreaterThan(0);
    const move = (await host.input('move')).events[0];
    expect(move.x).toBeCloseTo(0.5, 1);
    expect(move.y).toBeCloseTo(0.5, 1);

    await page.locator('#zoom-reset').click();
    await expect(page.locator('#zoom-pill')).toBeHidden();
    expect(await page.locator('#screens').evaluate((el) => el.style.transform)).toBe('');
  });

  it('on-screen keyboard text stays text and deletion becomes a key tap', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await page.locator('#keys').evaluate((el) => {
      el.dispatchEvent(new InputEvent('beforeinput', { inputType: 'insertText', data: 'a!', bubbles: true, cancelable: true }));
      el.dispatchEvent(new InputEvent('beforeinput', { inputType: 'deleteContentBackward', bubbles: true, cancelable: true }));
    });
    await expect.poll(() => keys(host, 'keydown')).toEqual([0x33]);
    expect((await host.input('text')).events.map((e) => e.text)).toEqual(['a!']);
  });
});

test.describe('clipboard and files', () => {
  it('syncs the clipboard in both directions', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/clipboard', 'copied on the host');
    await expect.poll(() => host.clientLog('mac clipboard changed').length).toBeGreaterThan(0);

    await host.clearInput();
    await page.evaluate(() => {
      const data = new DataTransfer();
      data.setData('text/plain', 'pasted from the client');
      document.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await expect.poll(() => host.get('/test/clipboard')).toBe('pasted from the client');
    expect((await host.input()).pastes).toBe(1);
  });

  it('temporarily focuses the hidden key field for paste on iPad', async ({ page, host }) => {
    await page.addInitScript(() => {
      // iPadOS uses a desktop Mac UA; unlike desktop browsers, screen clicks must not
      // focus the sink because that would exit fullscreen and summon the keyboard.
      Object.defineProperty(navigator, 'userAgent', { value: 'Mozilla/5.0 (Macintosh; Intel Mac OS X) AppleWebKit/605.1.15 Safari/605.1.15' });
      Object.defineProperty(navigator, 'maxTouchPoints', { value: 5 });
    });
    await open(page, host);
    await page.locator('#screens').click();
    expect(await page.evaluate(() => document.activeElement?.id)).not.toBe('keys');
    await page.keyboard.down('Meta');
    expect(await page.evaluate(() => document.activeElement?.id)).toBe('keys');
    await page.keyboard.up('Meta');
    expect(await page.evaluate(() => document.activeElement?.id)).not.toBe('keys');
  });

  it('offers to send the clipboard on return where reading it needs a tap (Safari)', async ({ page, host }) => {
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin: host.base });
    await page.addInitScript(() => {
      // Look like Safari: no userAgentData, so the clipboard can't be read without a tap.
      Object.defineProperty(Navigator.prototype, 'userAgentData', { get: () => undefined });
      localStorage.setItem('glass.clipboardSync', '1');
    });
    await open(page, host);
    await page.evaluate(() => navigator.clipboard.writeText('copied on the iPad'));
    await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
    const toast = page.locator('.toast', { hasText: 'Send it to the computer' });
    await expect(toast).toHaveCount(1);
    await toast.getByRole('button', { name: 'Send' }).click();
    await expect.poll(() => host.get('/test/clipboard')).toBe('copied on the iPad');
    expect((await host.input()).pastes).toBe(0);
  });

  // A 1×1 PNG.
  const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==';

  it('pastes rich text and an image on the host', async ({ page, host }) => {
    await open(page, host);
    await host.clearInput();
    await page.evaluate((png) => {
      const data = new DataTransfer();
      data.setData('text/plain', 'a caption');
      data.setData('text/html', '<b>a caption</b>');
      const bytes = Uint8Array.from(atob(png), (c) => c.charCodeAt(0));
      data.items.add(new File([bytes], 'image.png', { type: 'image/png' }));
      document.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    }, PNG);
    await expect.poll(async () => (await host.get('/test/clipboard/rich')).image).toBe(PNG);
    expect(await host.get('/test/clipboard/rich')).toMatchObject({ text: 'a caption', html: '<b>a caption</b>' });
    await expect.poll(async () => (await host.input()).pastes).toBe(1);

    // Rich text alone rides the control channel.
    await page.evaluate(() => {
      const data = new DataTransfer();
      data.setData('text/plain', 'plain');
      data.setData('text/html', '<i>plain</i>');
      document.dispatchEvent(new ClipboardEvent('paste', { clipboardData: data, bubbles: true, cancelable: true }));
    });
    await expect.poll(() => host.get('/test/clipboard/rich')).toEqual({ text: 'plain', html: '<i>plain</i>', image: null });
  });

  it('copies rich text and an image from the host with ⌘C', async ({ page, host }) => {
    await page.context().grantPermissions(['clipboard-read', 'clipboard-write'], { origin: host.base });
    await open(page, host);
    await fetch(`${host.base}/test/clipboard/rich`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ text: 'a caption', html: '<b>a caption</b>', image: PNG }),
    });
    await expect.poll(() => host.clientLog('mac clipboard changed').some((e) => e.data?.image)).toBe(true);
    await page.locator('#screens canvas').first().click();
    await page.keyboard.press('ControlOrMeta+KeyC');
    await expect.poll(() => host.clientLog('rich clipboard copied').length, { timeout: 10_000 }).toBeGreaterThan(0);
    const here = await page.evaluate(async () => {
      const [item] = await navigator.clipboard.read();
      const image = item.types.includes('image/png') ? await createImageBitmap(await item.getType('image/png')) : null;
      return {
        types: [...item.types].sort(),
        html: await (await item.getType('text/html')).text(),
        size: image && [image.width, image.height],
      };
    });
    expect(here.types).toEqual(['image/png', 'text/html', 'text/plain']);
    expect(here.html).toContain('<b>a caption</b>');
    expect(here.size).toEqual([1, 1]);
  });

  it('uploads files to the host', async ({ page, host }) => {
    await open(page, host);
    const body = Buffer.alloc(3 * 1024 * 1024, 7);
    await page.locator('#file-input').setInputFiles([
      { name: 'big.bin', mimeType: 'application/octet-stream', buffer: body },
      { name: '../../escape.txt', mimeType: 'text/plain', buffer: Buffer.from('hi') },
    ]);
    const files = async () => Object.fromEntries((await host.get('/test/downloads')).files.map((f) => [f.name, f.size]));
    // Uploads land under a .glass-partial name and are renamed when complete.
    await expect.poll(files, { timeout: 20_000 }).toEqual({ 'big.bin': body.length, 'escape.txt': 2 });
    // "../../escape.txt" had its path stripped: it landed in the downloads folder.
    await expect(page.locator('.toast', { hasText: 'Saved on' }).first()).toBeVisible();
  });

  it('downloads files picked on the host', async ({ page, host }) => {
    await open(page, host);
    await toolbarClick(page, 'btn-download');
    // The finished transfer's toast has the Save action (the progress toast doesn't).
    const toast = page.locator('.toast', { hasText: 'glass-test-pick', has: page.locator('.action') });
    await expect(toast).toBeVisible({ timeout: 10_000 });
    await expect(toast.locator('button', { hasText: 'Save' })).toBeVisible();
  });
});

test.describe('audio', () => {
  it('streams audio once turned on', async ({ page, host }) => {
    await open(page, host);
    await expect(page.locator('#btn-audio')).toBeVisible();
    await toolbarClick(page, 'btn-audio');
    // Opus (10 ms packets) where WebCodecs decodes it, else PCM (5 ms).
    await expect.poll(async () => (await stats(page)).audioPackets, { timeout: 10_000 }).toBeGreaterThan(75);
    const { audioCodec, audioPackets } = await stats(page);
    expect(audioPackets).toBeGreaterThan(audioCodec === 'opus' ? 75 : 150);
    expect(['pcm', 'opus']).toContain(audioCodec);
  });
});

test.describe('resilience', () => {
  it('recovers from lost frames with a keyframe', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/drop?frames=3');
    await expect.poll(async () => (await stats(page)).dropped, { timeout: 5_000 }).toBeGreaterThan(0);
    await expect.poll(async () => (await stats(page)).dropped, { timeout: 5_000 }).toBe(0);
    expect((await stats(page)).fps).toBeGreaterThan(10);
    expect(host.clientLog('decoder error detail')).toHaveLength(0);
  });

  it('a few lost frames recover without cutting the bitrate, and a quiet stream does not climb', async ({ page, host }) => {
    await open(page, host);
    const bps = async () => (await host.get('/test/bitrate')).bps;
    const before = await bps();
    await host.post('/test/drop?frames=4');
    // This viewer acknowledges frames, so its rate follows the queue (ack_rate tests in
    // flow.rs): isolated loss isn't congestion. What it needs is the picture going again.
    await expect.poll(async () => (await stats(page)).fps, { timeout: 5_000 }).toBeGreaterThan(10);
    await expect(page.locator('#hud')).toContainText('target');
    // The test pattern uses a sliver of its allowance: no evidence the link could take more.
    await page.waitForTimeout(4000);
    expect(await bps()).toBeLessThanOrEqual(before);
  });

  it('never decodes a corrupted frame', async ({ page, host }) => {
    await open(page, host);
    await host.post('/test/corrupt');
    await expect.poll(() => host.clientLog('frame checksum mismatch').length).toBe(1);
    await expect.poll(async () => (await stats(page)).fps).toBeGreaterThan(10);
    expect(host.clientLog('decoder error detail')).toHaveLength(0);
  });

  it('keeps streaming while a desktop tab is briefly hidden; an iPad pauses at once', async ({ page, browser, host }) => {
    const hide = (p) => p.evaluate(() => {
      Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
      document.dispatchEvent(new Event('visibilitychange'));
    });
    // Covered by another window, as on every Alt+Tab on Windows: switching back must not
    // wait for a keyframe and a new decoder.
    await open(page, host);
    await hide(page);
    await expect.poll(() => host.clientLog('visibility: hidden').length).toBe(1);
    await page.waitForTimeout(1500);
    expect(host.clientLog('video paused (tab hidden)')).toHaveLength(0);
    expect((await stats(page)).fps).toBeGreaterThan(10);

    const ipad = await browser.newPage({
      userAgent: 'Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1',
    });
    await open(ipad, host);
    await hide(ipad);
    await expect.poll(() => host.clientLog('video paused (tab hidden)').length).toBe(1);
  });

  it('reconnects after the host restarts', async ({ page, host }) => {
    await open(page, host);
    await host.stop();
    await expect.poll(async () => (await stats(page)).connected, { timeout: 10_000 }).toBe(false);
    await host.start();
    await expect.poll(async () => (await stats(page)).fps, { timeout: 20_000 }).toBeGreaterThan(10);
  });
});

test.describe('machines', () => {
  it('switches to another machine and falls back from a dead one', async ({ page, host }) => {
    // Shares the config dir, so the same token and certificate work on both.
    const other = await new Host({ configDir: host.configDir }).start();
    try {
      await open(page, host);
      await page.evaluate((id) => localStorage.setItem('glass.machine', id), `localhost:${other.wt}`);
      await page.reload();
      await expectVideo(page);
      await host.clearInput();
      await other.clearInput();
      await page.keyboard.press('a');
      await expect.poll(async () => (await other.input('text')).events.map((e) => e.text)).toEqual(['a']);
      expect((await host.input('text')).events).toHaveLength(0);

      // A machine that never answers: back home after a few tries or a timeout.
      await page.evaluate(() => localStorage.setItem('glass.machine', 'localhost:1'));
      await page.reload();
      await expect(page.locator('.toast', { hasText: 'back to this machine' })).toBeVisible({ timeout: 30_000 });
      await expectVideo(page);
    } finally {
      await other.stop();
    }
  });
});

test.describe('updates', () => {
  test('offers a newer release and asks the host to install it', async ({ page }) => {
    const host = await new Host({ latest: '9.9.9' }).start();
    try {
      await open(page, host);
      const toast = page.locator('.toast', { hasText: 'Update available: Glass 9.9.9' });
      await expect(toast).toBeVisible();
      await toast.locator('button', { hasText: 'Update' }).click();
      await expect.poll(async () => (await host.get('/test/updates')).count).toBe(1);
      await expect(page.locator('.toast', { hasText: 'Updating to 9.9.9' })).toBeVisible();
    } finally {
      await host.stop();
    }
  });

  it('offers nothing when up to date', async ({ page, host }) => {
    await open(page, host);
    await page.waitForTimeout(1000);
    await expect(page.locator('.toast', { hasText: 'Update available' })).toHaveCount(0);
  });

  test('reloads into new client code after an update', async ({ page }) => {
    // Its own copy of the web files: touching the shared ones would reload every other test's page.
    const { cpSync, mkdtempSync, utimesSync } = await import('node:fs');
    const { tmpdir } = await import('node:os');
    const webDir = mkdtempSync(`${tmpdir()}/glass-web-`);
    cpSync(new URL('../../web', import.meta.url), webDir, { recursive: true });
    const host = await new Host({ webDir }).start();
    try {
      await open(page, host);
      const { version } = await host.get('/config.json');
      // Simulate an update by bumping a web file's modification time.
      const now = new Date(Date.now() + 5000);
      utimesSync(`${webDir}/icon.svg`, now, now);
      expect((await host.get('/config.json')).version).not.toBe(version);
      const reloaded = page.waitForEvent('load', { timeout: 20_000 });
      await host.stop();
      await host.start();
      await reloaded;
      await expectVideo(page);
    } finally {
      await host.stop();
    }
  });
});
