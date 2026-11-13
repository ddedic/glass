# Viewer typing: behavior and validation

Addresses #146: the viewer resolves printable characters; the host receives text
instead of interpreting those keys through its own keyboard layout.

## Behavior

- Windows and macOS advertise `viewer_typing_v1`. Older/unsupported hosts retain
  physical input. Linux retains its existing physical and limited text fallback.
- Printable characters and committed IME/dead-key text use `InputEvent::Text`.
  Ctrl/Cmd/remote-Alt shortcuts and editing keys remain physical.
- Space and Shift+Space are deliberately physical for holds and panning. They follow
  the target application's composition behavior, not a literal-space guarantee.
- Physical keyboard override supports games and letter-based tools. On Mac viewers
  connected to Windows, Option-as-Alt changes Option chords independently of typing.
- Windows text uses UTF-16 Unicode packets, never layout-dependent punctuation taps.
  Partial insertion receives key-up-only recovery, never a replay of attempted text.

## Release ordering and ownership

One bounded scheduler serializes native input. Network readers keep accepting input
while text executes one scalar at a time. Releases of already-active owned presses
may overtake text; pending down/up pairs cannot invert. Local shortcut batches retain
their own order. Later queued viewer chords use the current modifier state.

Disconnect or overload cancels that owner's pending input. Already-inserted text
remains. Shared keys/buttons are released only when the final owner lets go. Native
uncertainty cancels affected work instead of replaying it on a different route.
Unsupported text ends its request without making the viewer's input stream unusable.

The Windows helper uses an independent, versioned request/ACK pipe with one native
step outstanding. Existing client/owner/version checks remain enforced. Focus and
desktop changes are best-effort boundaries: SendInput cannot pin input to an HWND.

## Completed verification

Final local verification after capability activation and removal of superseded unused
ownership code: 344 host + 15 protocol tests passed (one existing ignore), workspace
build passed, 51 JavaScript unit tests, 23 browser-input tests and 21 host-backed tests
passed. Native evidence below precedes those final activation/unused-code changes.

- Linux workspace: 347 host tests and 15 protocol tests passed; one existing speed
  test ignored. Workspace build passed before the final three review corrections;
  the corrected full test suite passed afterward.
- Browser input: 23 tests passed. Real host-backed input/clipboard/files: 21 passed,
  including same-stream release during text and disconnect without draining text.
- Corrected native Windows: normal build and 56 targeted Rust tests passed.
- Corrected native ARM64 Mac: normal build and 55 targeted tests passed.
- Windows production WebTransport/SendInput (no FakeInput): 50 checks passed against
  an owned receiver with Croatian KLID 0000041A. Unicode/surrogates, shared ownership,
  Shift/Ctrl transitions, held Enter, multiline input and EOF cancellation passed.
- With 4096 ASCII characters pending, Space-up arrived in 40.781–57.969 ms across
  three trials, before exact eventual text completion. These observations are not
  hard latency guarantees. Previously the same payload delayed release about 5 s.
- Existing Edge textarea: 16 checks passed, including trusted native events and
  exact Unicode/multiline/long documents. Full 4096-character documents took about
  12–13 s; responsive release is not a claim of fast bulk insertion.
- Owned Edge panning fixture: nine checks passed for physical Space+drag and
  Shift+Space+drag with trusted key/pointer events. This is not Figma/Photoshop testing.
- Native Windows pipe tests passed for duplex requests, deadlines, cancellation and
  EOF. These tests do not establish the real SYSTEM helper route.
- Review found and corrected local chord ordering, Mac nonmodifier reset, and Linux
  unsupported-text stream cancellation. Regression tests reproduced the defects;
  a fresh focused recheck found no remaining concrete findings within that scope.

Corrected native source identity: base `6864a78` plus frozen patch
`66c40d3d6d8f7e83230f3f5e21e6ef717033550eda5849cb69966a912e3deaf1` and correction
`659c78fed0b5640301e877d2080e1adce2fdeb87a00ae3bd4898beae766f123a`.
Windows binary: `2cfa46303fa3b067488397d87b7dde186cff1adf93837936e56e9cb5b1fc2935`.
Mac binary: `646a8697c55aaea9013087858b877a6b2f78930c80d2ba52c0eac3cfa09086e2`.

## Remaining validation and limits

Actual SYSTEM/elevated/secure-desktop routing remains unverified. Windows Sandbox
has no console-user identity, which the unchanged production authentication requires.
Guest service setup was removed, its firewall rules and SAS policy were restored,
and no host service/security settings were changed. No additional VM is required for
this PR; that validation limitation is explicit rather than reported as a pass.

Real Mac-German keyboard → Croatian Windows end-to-end testing, physical AltGr with
an existing hold, actual Caps-on combinations, and Figma/Photoshop remain manual
follow-up cases. Native builds and synthetic browser events do not prove them.
Windows Unicode packets do not guarantee acceptance in games or every application.
Concurrent physical keyboard input outside Glass ownership remains interference.

Suggested manual check: type `@ +!? čćžšđ Ł 😀`, dead-key/IME compositions and multiline
text; exercise shortcuts, held Space/Shift+Space, Option/AltGr and physical override;
repeat with Caps on each side, two viewers, reconnect and focus changes. Check
ordinary, elevated and secure-desktop targets separately. Never infer a pass from a
successful SendInput return alone; verify the resulting application text and holds.
