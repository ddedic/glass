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
| Mouse, keyboard, scroll | ✅ CGEvent | ✅ uinput | ✅ SendInput | Windows: keys by scan code |
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
| Managed deployment | ❌ | ❌ | ✅ per-machine MSI, 🧪 winget manifests | `msiexec /i glass-….msi /qn`; Program Files, starts for every user, firewall rule |
| Admin policies (locked settings) | ❌ | ❌ | ✅ Group Policy / Intune (ADMX) | HKLM/HKCU\SOFTWARE\Policies\Glass; packaging/windows/policies |
| Background service | ✅ LaunchAgent | ✅ systemd user | ✅ glassw at sign-in | |
| Secure-desktop service (SYSTEM) | — | — | ✅ optional | `glass service install`; keeps a SYSTEM helper in the console session for the lock screen, UAC and Ctrl+Alt+Del. Host stays the only network/auth endpoint |
| Self-update | ✅ | ✅ | ✅ | Windows MSI installs update with the MSI or winget; a policy can turn updates off |
| Guided setup | ✅ | ✅ | ✅ (unattended) | Windows adds a firewall rule (private networks; Tailscale-only when `glass config bind` is tailscale) and offers the secure-desktop service (one UAC prompt each) |
| Desktop app | ✅ Glass.app | ❌ | ✅ Start menu | Same window |
| Status icon (who's viewing, quick controls) | ✅ menu bar | ❌ | ✅ notification area | |
| CLI ↔ host channel | ✅ Unix socket | ✅ Unix socket | ✅ named pipe | |
| Fleet reports | ✅ | ✅ | 🧪 | |
| Fleet scripts | ✅ | ✅ | 🧪 .ps1/.cmd/.bat/.exe | |
| Headless (no monitor) | — | 🚧 | ❌ | |
| Recording to MP4, port forwarding, links, SSO/LDAP/SCIM, posture, relay, console | ✅ | ✅ | 🧪 | Platform-independent code. Windows: recording and one-time links work in the VM |

## Windows Firewall

Glass adds one inbound allow rule named **Glass** for each program that listens: the per-user `glass.exe`, the secure-desktop copy at `C:\Program Files\Glass\Service\glass.exe` (that copy is also the sign-in host), and `bin\glass.exe` from a per-machine MSI. `glassw.exe` does not listen and gets no rule. A rule with any other name is left alone, including outbound rules and block rules. Allow rules combine, so a wider allow under another name (for example the one Windows creates if its own prompt was accepted) still lets those addresses in. Glass says so and does not remove it.

The rule follows the saved `glass config bind`, not a one-off `--bind`. The firewall sees the other computer's address, so listening on this machine's Tailscale address still allows every Tailscale peer. An elevated reconcile reads every user's saved bind from that profile's real roaming AppData (including a redirected folder) and the sign-in copy under `%ProgramData%\Glass\signin`, and uses the narrowest: loopback, then Tailscale's two ranges, then any remote address. A settings file that is missing is the default bind. A file or profile that cannot be read is not success: Glass removes the allow rules it owns and does not create a wide one. `glass update` and the Windows install script apply the same change, including when the secure-desktop service is not installed, and do not report the update finished if that step did not succeed.

| Saved bind | Inbound rule on the private profile |
|---|---|
| Default, `0.0.0.0`, `::`, or any LAN, VPN, or public address | Any remote address. This is the rule Glass has always created. A specific address outside Tailscale stays this wide: the private profile is the existing boundary, and those peers are not one fixed range. `localsubnet` would drop clients that reach the computer through a route or another VPN. |
| `tailscale`, or a Tailscale address (`100.64.0.0/10` or `fd7a:115c:a1e0::/48`), plus optional loopback | Only `100.64.0.0/10` and `fd7a:115c:a1e0::/48`. Edge traversal is blocked. |
| Only `127.0.0.1` / `::1` | No Glass allow rule. The firewall does not filter loopback. |

Mixing a Tailscale address with any other non-loopback address uses the wide rule, because that other address accepts more than the tailnet.

`glass config bind` does not ask for administrator approval. When the process is already elevated it updates the rule; otherwise it prints that `glass setup` will. `glass setup` asks once, as before, and updates every Glass program above, including the per-user install even when setup was started from another copy. `glass service install` does the same update while it is already elevated (the installer runs it again when the service version changes). Running any of them again replaces a stale or wider rule named Glass for that program instead of adding a duplicate. A rule stored with edge traversal "defer to user" cannot take a remote-address filter, so Glass deletes that rule and creates a new one.

If the Tailscale or loopback rule cannot be created, Glass still removes the wider allow it owns and reports the failure. Connectivity can be lost; the wider allow is not left behind. A block rule does not stop that removal, and Glass does not delete block rules while syncing (uninstall still removes the Glass rules for that program). A rule with any other name is left in place.

The per-machine MSI does not install a firewall exception. After the files are copied it runs `glass --glass-firewall-sync` as SYSTEM, which writes the rule for the narrowest saved bind. That runs on install and on upgrade, so an upgrade does not put back a wide exception. The rule is one per program: on a shared install the narrowest saved bind is the one that copy keeps. A block rule from Windows' own prompt is left alone and reported.
