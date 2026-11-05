# Viewer character typing implementation plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Fix #146: viewer characters appear on Windows/macOS hosts while shortcuts retain their physical positions.

**Architecture:** Reuse ordered `InputEvent::Text` records. Classify text using the viewer's resolved key and platform; defer modifiers until a physical action needs them. Keep an explicit physical keyboard mode and Linux's existing behavior. Reuse native injectors and extend the Windows helper to receive text on its existing ordered input channel.

**Tech Stack:** Browser JavaScript, Node tests, Playwright, Rust, Windows SendInput, macOS CGEvent.

**Spec:** The investigation and design in this conversation, approved for implementation by “Can we then plan implementation and start??”.

## Global constraints

- Work in this checkout on `fix/viewer-character-typing`; do not open PRs or post comments.
- No clipboard substitution, layout switching, new network event, or dependencies in production.
- Preserve clipboard shortcuts, modifier remappings, editing keys, input permissions, and ordered delivery.
- Linux remains physical by default; mobile composition retains its existing focus behavior.
- Native Windows/macOS behavior requires native validation; Linux browser tests cannot certify it.

## Review focus

- Option/AltGr typing must not trigger host shortcuts or leave modifiers down.
- Composition commits must appear once; cancelled/dead-key sequences must not leak physical keys.
- Modifier transitions, pointer chords, repeats, focus loss and mode switches must release only transmitted keys.
- Elevated/secure desktop routing must keep text and keys ordered without US-layout fallback.
- Existing keyboard profiles and physical game controls must remain usable.

### Task 1: Viewer routing and regression tests

**Files:** `web/input.js`, new `web/typing.js`, `tests/unit/typing.test.mjs`, browser regression tests under `tests/e2e/`.

**Interfaces:** `typedKey(event, { macViewer, commandHeld })` returns a printable character or undefined. Modifier state keeps locally held keys distinct from transmitted keys. `attachInput().setPhysicalKeyboard(on)` resets the routing state.

- [x] Add failing tests for ASCII, German Mac Option+L, AltGr, shortcut precedence, unknown/dead keys and physical mode.
- [x] Run the tests and confirm expected failures.
- [x] Implement classification and deferred modifier handling; use Text for committed text on supported hosts.
- [x] Add integration coverage for emitted records, repeat, composition, pointer modifiers, blur, and mode changes.
- [x] Run Node and browser tests; correct regressions.

### Task 2: Physical keyboard preference

**Files:** `web/keyboard-profile.js`, `web/main.js`, `web/ui.js`, `tests/unit/keyboard-profile.test.mjs`.

**Interfaces:** profiles gain `physical: boolean`; UI calls `onPhysicalKeyboard(on)` and reflects profile changes.

- [x] Add failing profile tests for defaults, persistence and migration from old profiles.
- [x] Implement the toggle and per-machine storage. Disable text routing on Linux independently of saved preference.
- [x] Verify profile and viewer tests.

### Task 3: Native text injection and Windows helper

**Files:** `crates/glass-host/src/windows/input.rs`, `windows/secure.rs`, `macos/input.rs`; native unit tests adjacent to affected code.

**Interfaces:** existing `InputSink::type_text` and `InputEvent::Text`; helper text reassembly is scoped to its input connection.

- [x] Add tests for text event routing/reassembly and pure native event construction where runnable.
- [x] Isolate text from held modifiers, validate Windows layout synthesis against Caps Lock, preserve Unicode pacing and UTF-16 character boundaries.
- [x] Forward complete text through the helper's ordered input channel and inject it on its input thread; clear incomplete text on release/disconnect.
- [x] Run Rust tests/builds available in this environment and explicitly record unavailable platform checks.

### Task 4: Verification and review

- [x] Run all Node unit tests and targeted browser regression tests; run Rust checks where toolchains are available.
- [x] Review the full diff, fix findings with regression coverage, and check whitespace/status.
- [x] Document manual Mac German → Windows Croatian tests (Notepad, browser, terminal, elevated Notepad, UAC/lock), physical games, Caps Lock, dead keys, emoji, and macOS hosts.
- [x] Leave the branch locally reviewable; no GitHub writes.

## Execution notes

- Starting point: `9339ce4` (v0.14.0), clean checkout.
- Existing 17 keymap/protocol unit tests pass.
- Initial environment discovery: Chrome is available; `rustup` is not on PATH. Native toolchain availability still to be checked.

## Implementation result

- Viewer routing, deferred modifiers, per-machine physical mode, host text isolation,
  Windows helper forwarding and updated integration expectations are implemented locally.
- Independent review found desktop-routing cleanup, stale Caps Lock state and touchscreen
  composition focus issues; all three were addressed. The touchscreen reproduction failed
  before its fix and passes afterwards.
- Ruling: already-transmitted modifiers stay held in the viewer until keyup; native text
  injection isolates/restores them. Releasing Alt in the viewer could activate host menus.
- Ruling: Windows only synthesizes layout keys whose result matches under both Caps Lock
  states. Other characters use paced Unicode; no foreground keyboard state is guessed.
- Verification: 49 Node unit tests and 13 standalone Chrome browser tests passed; 15 Rust
  protocol tests passed in `rust:slim`. Two native pure-function tests passed in a harness
  extracted from the actual source (helper packet reassembly and macOS UTF-16 chunking).
- Rust syntax parsing passed for all three changed native files. This is not a type check.
- Full `cargo test --workspace --locked` passed in an isolated Linux container with
  PipeWire development dependencies: 292 host tests and 15 protocol tests passed, with
  one host test ignored. `cargo build --locked` also passed. These Linux checks do not
  compile the Windows/macOS input modules. The full host-backed Playwright suite was not run.
- Follow-up review found active-drag modifier delivery and stale text-key ownership after
  a swallowed macOS keyup; both were fixed with browser regression coverage.

### Still required before release

- [ ] Native Windows and macOS compilation/tests.
- [x] Targeted host-backed Playwright integration suite (input, clipboard/files and machine switching). The complete unrelated E2E suite was not run.
- [ ] Live cross-layout and secure-desktop validation in `docs/keyboard-typing-validation.md`.

## 2026-11-06 follow-up

- User authorized installation of all required build dependencies. Built isolated Docker
  image `glass-typing-build` with pkg-config, PipeWire headers, clang, cmake, nasm and
  standard build tools; the Rust workspace tests and host build are running with a
  persistent target/cache. The previous missing-dependencies blocker is resolved.
- User requested Astra reviews in a regular reusable T3 sidebar session. In review round
  1 Astra reproduced delayed modifier delivery during an active drag and stale text-key
  ownership after a swallowed macOS Command chord keyup. Both regressions were observed
  failing and fixed; all 13 browser tests and 49 Node tests now pass.
- Astra review round 2 found no remaining blocking code defects. Platform runtime release
  gates remain; review is not evidence of native OS input behavior.
- Targeted host-backed integration tests for input, clipboard and machine switching are
  queued after the workspace test/build job. Results will be recorded after completion.

### Dependency/build verification result

- The full Rust workspace run passed: 292 host tests plus 15 protocol tests; the existing
  `soft_encode_4k_speed` benchmark remained ignored. `cargo build -p glass-host --locked`
  passed as well. Dependencies are installed in the isolated image, not on the host OS.
- The first host-backed integration run exposed an obsolete desktop-focus assumption in
  the Safari paste test. The test now identifies itself as iPad (Mac UA plus touch points),
  retaining temporary paste focus there while desktop composition keeps its editable sink.

### Final automated results (2026-11-06)

- `cargo test --workspace --locked`: 307 tests passed (292 host + 15 protocol), 1 existing
  ignored 4K speed benchmark. `cargo build -p glass-host --locked`: passed.
- `node --test tests/unit/*.test.mjs`: 49 passed.
- `npx playwright test input.spec.mjs`: 13 passed.
- Targeted host-backed `glass.spec.mjs` run for input, clipboard/files and machines:
  19 passed in 1.8 minutes on a clean rerun with its own output directory. The prior run's
  desktop-focus assumption was corrected to explicitly emulate iPad; two prior cleanup
  errors were caused by overlapping Playwright output directories, resolved in the rerun.
- Astra second review: no remaining blocking code defects. Both first-round findings
  have failing-then-passing browser regression coverage.
- Native Windows/macOS compilation and real cross-layout/secure-desktop input validation
  remain release gates. Linux test-mode hosts verify transport/behavior, not OS injection.
- Final diff whitespace check passed. Changes remain local/uncommitted; no GitHub writes.
