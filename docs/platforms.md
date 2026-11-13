# Host features by platform

What each host OS can do, to track what's done and what's left. Viewer-side features
(the web client, PWA, keyboard profiles, push) work the same for every host and aren't listed.

✅ works · 🧪 built, not yet run on real hardware · 🚧 being built · ❌ not yet · ? unverified · — doesn't apply

Windows shipped in 0.13.0 (beta); a ✅ there means it ran in a Windows 11 Arm VM (UTM, no GPU), driven from a browser: the published x64 (under emulation) and Arm64 builds pass the VM suite (tests/e2e/windows-vm, 18 tests). Nothing has run on a real PC yet.

## Picture

| Feature | macOS | Linux | Windows | Notes |
|---|---|---|---|---|
| Capture every display | ✅ ScreenCaptureKit | ✅ portal + PipeWire | ✅ DXGI Desktop Duplication | |
| Pointer in the picture | ✅ | ✅ | ✅ drawn on the CPU | |
| Hardware H.264 | ✅ VideoToolbox | ✅ VA-API, opt-in | 🧪 Media Foundation | Windows falls back to OpenH264 |
| Software H.264 (OpenH264) | — | ✅ | ✅ | |
| Up to 120 fps | ✅ | ❌ 60 | 🧪 with hardware encode | |
| HEVC, sharp text 4:2:2 | ✅ | ❌ | ❌ | |
| Lossless 4:4:4 still picture | ✅ | ✅ | 🧪 | |
| Display P3 | ✅ | — | ❌ | |
| Agent screenshots | ✅ | ✅ | ✅ | |
| Display previews | ✅ | ✅ | 🧪 | Windows: the test VM has one display |
| Rotated displays | ✅ | ✅ (Hyprland) | 🧪 | Windows sends them unrotated today |
| Displays plugged in later | ✅ | ❌ restart | 🧪 | |
| Stream one window | ✅ | ❌ | ✅ | |
| Device as an extra display | ✅ virtual display | ❌ | ❌ | Windows needs an IddCx driver |
| Reshape the desktop to the viewer | ✅ | ❌ | ❌ | |

## Control

| Feature | macOS | Linux | Windows | Notes |
|---|---|---|---|---|
| Mouse, keyboard, scroll | ✅ CGEvent | ✅ uinput | ✅ SendInput | Windows/macOS: viewer text, physical shortcuts; More → Physical keyboard for games/host layout. Linux: physical keys. |
| Relative mouse (games) | ✅ | ✅ | 🧪 | |
| Type any text (agents) | ✅ | ❌ | ✅ layout keys, Unicode for the rest | Accents, symbols, CJK |
| Pen pressure and tilt | ✅ | ❌ | 🧪 synthetic pen | |
| Game controllers | ❌ | ✅ uinput | 🧪 ViGEmBus | Windows: a virtual Xbox 360 controller when ViGEmBus is installed |
| Use the lock screen / sign-in screen (while signed in) | ✅ | ? | ✅ SYSTEM helper | Windows: `glass service install` keeps a SYSTEM helper on the input desktop |
| Control elevated windows, UAC prompts | — | — | ✅ SYSTEM helper | Same service; input passes UIPI through SYSTEM |
| Sign in after a restart (nobody signed in yet) | ❌ | ❌ | ✅ sign-in host | Windows: the same service serves the sign-in screen (screen and input only) until someone signs in, then the user's own host takes over |
| Send Ctrl+Alt+Del | — | — | ✅ SendSAS | Viewer's More menu. Needs the secure-desktop service (sets the SoftwareSASGeneration policy) |
| Wake on LAN, restart Glass/computer | ✅ | ✅ | 🧪 | Windows: restarting Glass works in the VM |
| Stay awake with the lid closed | ✅ | ✅ | 🧪 power plan lid action | Windows: "Do nothing" on power while it's on, put back after |

## Sound, clipboard, files

| Feature | macOS | Linux | Windows | Notes |
|---|---|---|---|---|
| System audio | ✅ | ✅ PipeWire | ✅ WASAPI loopback | Windows: checked by level, with VB-CABLE as the VM's output; the viewer says when a computer has no sound output |
| Viewer's microphone | ✅ Glass Microphone driver | ✅ virtual source | ✅ Glass Microphone driver, 🧪 VB-CABLE | Windows: our driver works in the VM, test-signed (native/glass-mic-windows), but needs Microsoft's signature to ship; releases use VB-CABLE until then |
| Now playing + media controls | ✅ | ✅ playerctl | 🧪 media session API | |
| Clipboard text | ✅ | ✅ | ✅ | |
| Clipboard rich text (HTML) | ✅ | ❌ | ✅ CF_HTML | |
| Clipboard images | ✅ | ✅ | ✅ PNG + bitmap | |
| File transfer, folders | ✅ | ✅ | ✅ | Windows: folders not yet tried |
| File browser | ✅ | ✅ | ✅ | |
| Move to Trash | ✅ | ✅ gio | ✅ Recycle Bin | |
| File previews | ✅ Quick Look | ❌ | ✅ shell thumbnails | |
| Pick files on the host | ✅ | ✅ portal | ✅ Open dialog | |

## People and agents

| Feature | macOS | Linux | Windows | Notes |
|---|---|---|---|---|
| Chat, notifications, prompts | ✅ | ✅ | ✅ toasts, message boxes | Windows: prompts not yet tried |
| "Someone is viewing" notice | ✅ app | ✅ | ✅ | |
| Drawings shown on the host | ✅ overlay | ❌ | ❌ | Windows could reuse the privacy overlay trick |
| Privacy screen | ✅ | ❌ | ✅ capture-excluded overlay | |
| Quick support (`glass support`, one-liner) | ✅ | ✅ | ✅ `irm glassdesk.dev/help \| iex` | Windows asks about the firewall once per session (Cancel is fine) |
| Terminal in the viewer | ✅ | ✅ | ✅ ConPTY | |
| Agent screenshots, clicks, keys | ✅ | ✅ | ✅ | |
| Agent `snapshot` (window as text) | ✅ Accessibility | ❌ | ✅ UI Automation | |
| Agents wait while you use it | ✅ | ❌ | 🧪 | |
| Agents kept out of password fields | ✅ | ✅ lock screen only | 🧪 lock screen only | |
| Front-app rules for agents | ✅ | ❌ | 🧪 | |

## Running it

| Feature | macOS | Linux | Windows | Notes |
|---|---|---|---|---|
| One-line install | ✅ install.sh | ✅ install.sh | ✅ install.ps1 | `irm glassdesk.dev \| iex` |
| Release builds | ✅ universal | ✅ x86_64 | ✅ x64 + Arm64 zips and MSIs | 0.12.0-windows.1 (pre-release): both pass the VM suite as published |
| Signed builds | ✅ stable identity | — | 🧪 Authenticode, timestamped | Windows: Azure Artifact Signing, a .pfx or SignPath (release.yml); unsigned until one is set up |
| Managed deployment | ❌ | ❌ | ✅ per-machine MSI, 🧪 winget manifests | `msiexec /i glass-….msi /qn`; Program Files, starts for every user. The MSI restores one machine-wide FirewallScope (the older private any-address rule when none is saved; that fallback is not stored as `any`). The last `glass setup` wins; each user's bind is separate. A full MSI uninstall clears the scope. `glass uninstall --everything` clears it only when Glass is not installed for everyone. After changing bind, run `glass service install` again so the service copy's rule matches |
| Admin policies (locked settings) | ❌ | ❌ | ✅ Group Policy / Intune (ADMX) | HKLM/HKCU\SOFTWARE\Policies\Glass; packaging/windows/policies |
| Background service | ✅ LaunchAgent | ✅ systemd user | ✅ glassw at sign-in | |
| Secure-desktop service (SYSTEM) | — | — | ✅ optional | `glass service install`; keeps a SYSTEM helper in the console session for the lock screen, UAC and Ctrl+Alt+Del. Host stays the only network/auth endpoint |
| Self-update | ✅ | ✅ | ✅ | Windows MSI installs update with the MSI or winget; a policy can turn updates off |
| Guided setup | ✅ | ✅ | ✅ (unattended) | Windows adds a firewall rule from the saved bind (Tailscale ranges only when bind is tailscale; none when loopback-only) and offers the secure-desktop service (one UAC prompt each) |
| Desktop app | ✅ Glass.app | ❌ | ✅ Start menu | Same window |
| Status icon (who's viewing, quick controls) | ✅ menu bar | ❌ | ✅ notification area | |
| CLI ↔ host channel | ✅ Unix socket | ✅ Unix socket | ✅ named pipe | |
| Fleet reports | ✅ | ✅ | 🧪 | |
| Fleet scripts | ✅ | ✅ | 🧪 .ps1/.cmd/.bat/.exe | |
| Headless (no monitor) | — | 🚧 | ❌ | |
| Recording to MP4, port forwarding, links, SSO/LDAP/SCIM, posture, relay, console | ✅ | ✅ | 🧪 | Platform-independent code. Windows: recording and one-time links work in the VM |
