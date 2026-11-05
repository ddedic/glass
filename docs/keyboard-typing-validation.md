# Character typing validation (#146)

For Windows/macOS hosts, viewers send resolved printable characters through the existing
Text input stream. Shortcuts and editing keys retain physical positions. More → Physical keyboard
restores physical typing for games and applications that require held keys. The preference
is saved per computer. Linux hosts retain their existing physical input behavior.

## Automated coverage

- `node --test tests/unit/*.test.mjs`: classifier, profiles and existing protocol tests.
- `cd tests/e2e && npx playwright test input.spec.mjs`: actual browser handlers and UI,
  observed at their input-record boundary; no native host required.
- `scripts/test.sh e2e`: existing native test-host suite, with updated text expectations.
- Native Rust unit tests cover helper text packet reassembly and macOS UTF-16 chunk
  boundaries. These are not tests of OS delivery.

## Native release gate (not yet verified)

1. Mac viewer with German input source, Windows host with Croatian layout: type
   `@ [] {} \\ | ~ ^ € ä ö ü ß č ć š ž đ`, mixed case, spaces, emoji and repeated letters.
   Confirm exact output in Notepad, a browser field and a terminal.
2. Repeat with host Caps Lock both on and off and viewer Caps Lock both on and off.
   Verify the viewer-resolved case wins and the host lock state is not changed by Text.
3. Test Ctrl/Cmd shortcuts, copy/paste, Shift+arrows, Alt+Tab, modified clicks/drags,
   an Option character after a modified click, and release after window/tab focus changes.
4. Compose accents using real dead keys in Safari and Chrome. Test dead key + space,
   cancelled composition, consecutive identical characters after composition, Japanese
   IME and mobile keyboards. Synthetic CompositionEvents do not prove OS sequencing.
5. On a PC viewer, test real AltGr and a separate Ctrl+Alt shortcut. On iPad, test
   existing Command/Option swapping and Ctrl-as-Command app switching.
6. Repeat text and shortcut tests in elevated Notepad, the lock screen and UAC with the
   secure-desktop helper installed. Switch between ordinary/elevated windows while holding
   modifiers or a mouse button. Without the helper, normal UIPI restrictions still apply.
7. Check physical mode with a game/key-driven application: key holds, repeats, release,
   pointer lock, and preference persistence after reconnecting or switching computers.
8. Repeat ordinary typing and shortcuts against a macOS host. Verify an emoji spanning
   a 16-unit text chunk is intact, and held modifiers do not transform committed text.

## Known constraints

- Native injection compatibility varies by application; physical mode is the escape hatch.
- Windows retains its 40 ms Unicode pacing. Case-sensitive characters use Unicode because
  an injection worker cannot reliably read the foreground app's Caps Lock queue state.
  Measure sustained typing/repeat latency before reducing the existing pacing.
- Input arriving from another local/remote keyboard concurrently can still interfere.
- Browser- or OS-reserved shortcuts cannot be recovered by the page.

## Verified in this checkout (2026-11-06)

The isolated Linux build passed all 307 Rust workspace tests (one pre-existing ignored 4K
speed benchmark) and produced the host binary. All 49 JavaScript unit tests, 13 standalone
Chrome input tests, and 19 host-backed tests for input, clipboard/files and machine
switching passed. Astra reviewed two rounds; its two initial findings were reproduced and
fixed, and the second review found no remaining blocking code defects.

The native release checklist above is still pending. These results do not certify Windows
or macOS compilation, Safari dead-key sequencing, or native elevated/secure-desktop input.
