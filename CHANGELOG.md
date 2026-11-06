# Changelog

What changed in each release of Glass. `glass update` (or the Glass app) installs the newest; every build is on [GitHub Releases](https://github.com/Ntarasiuk/glass/releases).

## Unreleased

- If Glass starts before Tailscale, as it can at boot, it no longer stays on plain HTTP and out of reach of the tailnet until someone restarts it. For five minutes it watches for Tailscale, then restarts itself with its HTTPS certificate as soon as nobody is connected. On Windows this covers the background app and the sign-in screen; on macOS and Linux, the background service once `glass setup` has run again. `glass` run in a terminal stays as it is.
- On Windows, the Glass firewall rule follows `glass config bind`. `tailscale` allows only 100.64.0.0/10 and fd7a:115c:a1e0::/48. A loopback-only bind removes the rule. The default listen address is unchanged: private networks, any remote address. `glass setup` and `glass service install` apply it. An elevated `glass config bind` applies it; otherwise Glass says to run `glass setup`. A silent install or upgrade restores the saved scope; with none saved it restores the older private-network rule, and a firewall failure does not stop the install. One existing Glass rule is updated in place; several rules with that name are replaced by one. A failed update of a single rule leaves it as it was. The any-address fallback is not stored, so a later `glass service install` with no scope leaves that copy's rule alone. A saved loopback scope adds no rule and says so. `glass uninstall --everything` clears the saved scope only when Glass is not installed for everyone; a full MSI uninstall clears it.
- The picture no longer freezes while you're using a page the browser wrongly reports as hidden (Edge on Windows did this under an always-on-top overlay, for minutes at a time). Input now counts as the page being on screen: video keeps going, and resumes if it had paused. A tab that's really in the background still pauses as before.

## 0.14.0

November 4, 2026

### Faster on slow links

- **The first picture on a slow link usually takes about a second, not ten.** Lost video fragments are sent again instead of whole new keyframes piling onto a full queue. A new viewer's first keyframe goes out at its screen's size, and viewers far away start at a lower bitrate and climb quickly. Sharp text turns on after the first picture.
- Sound from the computer takes about a tenth of the bandwidth: browsers that can decode Opus (Chrome, Edge, Firefox, Safari 26) get it as Opus at 96 kbps instead of 1.5 Mbps of raw audio, so on a slow or relayed connection (a car's browser) sound no longer crowds out the picture. Older Safari keeps getting raw audio.
- On a slow or lossy link, the picture no longer sticks waiting for a keyframe: while a viewer's connection is still busy with the last frames, Glass waits before capturing the next one, and that frame then carries everything that changed. Fewer frames get through, but none are dropped.
- On a slow link, below 3 Mbps frames may be coarser so they fit, instead of huge ones holding up everything behind them; a still screen sharpens afterwards. On Windows and Linux hardware encoders, no single frame takes much more than its share of the bitrate.
- A still screen sharpens without slowing down what comes next on a slow link: on the Mac, Glass refines it a few times (fewer and smaller the lower the bitrate) and only once the frames before have arrived, instead of re-sending the whole picture a dozen times a second for 3 seconds.
- The viewer tells Glass when each frame is on screen. Glass uses that to send the next frame only when there's room, and to set the bitrate ten times a second from how long frames take to arrive and how much gets through: it backs off before a queue builds, and finds a fast link's rate within a second or two. It can go as low as 0.25 Mbps when that's all the link carries, but a keyframe arriving, a hidden tab, or the delay swings of a phone network don't count against it. Older viewers keep the once-a-second adjustment.
- Viewers whose browser can't take WebRTC video (a Tesla's, for one) on a slow or relayed connection get a steadier picture: Glass keeps about a tenth of a second of video waiting and lowers the bitrate to what the link carries, instead of queuing most of a second and dropping frames in bursts.

### Security

- Web pages on other sites can no longer drive or read your computer through a browser on one of your devices. Glass answers only for its own names (its tailnet name, IP addresses, localhost; `glass config allowed-hosts` adds more), so a site can't point its own domain at it (DNS rebinding), and it turns away requests and WebTransport sessions from other sites' pages (`glass config allowed-origins` lets one in). A refused request gets a 403 that says why. `POST /agent/v1/act` now needs `Content-Type: application/json`.

### Using the remote computer

- On a Mac or Windows PC, the pointer moves with your mouse at once instead of trailing it by the connection's delay, and moving it no longer costs a video frame: your browser draws it, at the size the screen is shown. When someone or something else moves it (another viewer, an agent, the person at the computer) you still see where it is. It stays in the video while it's needed there: during a recording, in trackpad mode, for an older viewer, and on Windows while an agent works. Linux keeps the pointer in the video.
- Scrolling keeps up with your fingers on a phone or tablet: two-finger and wheel scrolling move the remote content as far as they move it on your screen, whatever size the remote display is shown at (before, a Mac shown at a quarter size scrolled about four times too slowly). On a Mac, slow trackpad scrolling no longer gets lost to rounding, and apps scroll as smoothly as with a trackpad.
- Chinese, Japanese and Korean input methods, emoji, and accented letters from a phone or tablet keyboard now type on the remote computer (before, they were dropped). On Linux hosts, characters a US keyboard doesn't have still can't be typed.
- The file browser downloads folders: select one and press Download to get it as a zip, made as it downloads (nothing is staged on the computer, however big the folder). On the computer's own page you can also drag a folder out. Symlinks inside the folder are left out.

### Agents

- Agents can record the screen (`record`): for some seconds, or started and stopped by id. It's the video Glass already encodes, saved as an MP4 without re-encoding, at most 10 minutes. The agent gets the MP4 back (when it's 20 MB or less) and its path on the computer; recordings are removed after a day.
- Agents' `wait_for_change` follows what capture says changed instead of comparing small screenshots, so a caret, a spinner or one text field changing counts. It waits for the screen to settle (`idle_ms`), or for a change first (`change`), in part of the screen if asked (`region`). Screenshots and actions return a `seq` that a wait takes as `since`, so a change that happened before the wait isn't missed. A Mac display no viewer is watching is followed while an agent uses it, and stops a minute after.

### Reliability

- If macOS's screen capture service stops answering (it can after Glass restarts), Glass restarts it and carries on, instead of hanging with no picture until someone restarts the Mac. Tailscale commands Glass runs at startup time out instead of holding it up.
- Letting someone in after more than 10 seconds no longer leaves them with a picture but no mouse or keyboard.
- `glass support` on a computer already running Glass shares through it, instead of starting a second copy that fought it for the screen (the picture froze in one of them). Ending support removes the helper's access and nothing else.
- If `glass support` is closed without ending the session (killed, crashed), Glass removes the helper's access within seconds.
- Your own tailnet devices connect without a token even when Tailscale isn't ready when Glass starts (common right after signing in to Windows). Glass checks again on the next connection and then remembers the owner. For a minute and a half after startup, a viewer who didn't bring a token or a link reconnects instead of seeing the token screen.

### For developers

- `bench/` measures frame rate and end-to-end latency on a test host (no screen capture) through LAN and slow shaped links, to catch regressions. See `bench/README.md`.

## 0.13.1

November 3, 2026

### Windows

- Glass has its icon on Windows: the app's window, its taskbar button and Alt+Tab, the programs in Explorer, the Start menu and Settings › Apps. Before, most of them showed Windows' blank icon.

### Website

- glassdesk.dev shows Windows visitors the PowerShell install command (`irm glassdesk.dev | iex`), with a switch to the Mac and Linux one. The Mac and Linux installer, run from Git Bash or WSL on Windows, points to it instead of installing the wrong build.

## 0.13.0

November 3, 2026

### Windows host (beta)

Glass now runs on Windows 10 (2004 or newer) and 11, on x64 and Arm. In PowerShell:

```powershell
irm glassdesk.dev/install.ps1 | iex
```

That installs it for your user, no administrator needed. For installing on every computer you manage there's an MSI (`msiexec /i glass-….msi /qn`) and Group Policy settings.

- Every display, with the pointer, or one window on its own. Mouse, keyboard, scroll, pen, and game controllers (with ViGEmBus).
- Copy and paste both ways (text, rich text, images); sending files and the file browser; recording; one-time links; chat and notifications; the computer's sound.
- With `glass service install`: the lock screen, UAC prompts, elevated windows and Ctrl+Alt+Del, and **signing in from the viewer after a restart**, with the same page going from the sign-in screen to your desktop.
- AI agents: screenshots, clicks, keys and typing any text.
- The viewer's microphone through VB-CABLE (Glass opens its installer).
- Updating (`glass update`, or the install command again) also updates the secure-desktop service, with one Windows permission prompt, so the lock screen keeps working.

It's a beta: tested on Windows 11 in a VM, not yet widely on real PCs, and the programs aren't code-signed yet, so SmartScreen may warn before the installer runs. [Report problems](https://github.com/Ntarasiuk/glass/issues).

### People on your tailnet

- `glass acl add friend@gmail.com` (or `@domain`) lets that person's Tailscale devices in without the token, as a guest with the rule's limits. Before, only your own devices skipped the token.
- `glass config tailnet-members on` (or **Let everyone on this tailnet in** in the app) lets everyone on the tailnet in as a guest. Off by default.

### Also

- The viewer's stats show how loud the sound reaching it is.

## 0.11.1

November 2, 2026

### Clipboard from iPad and iPhone

- ⌘V on an iPad with a keyboard now pastes what's on the iPad, as it does elsewhere. Before, Safari didn't hand Glass the clipboard, and the Mac pasted whatever was already on its own.
- Sync clipboard now works in Safari too (turn it on in the menu): when you come back to Glass after copying in another app, a Send button puts it on the Mac's clipboard. Safari asks you to tap Paste to confirm; Chrome and Edge still send it without asking.

## 0.11.0

November 2, 2026

### Linux on ARM

- Glass now runs on ARM64 Linux (aarch64) as well as x86_64: ARM laptops and desktops, cloud servers, or a Raspberry Pi with a recent distribution. Like the x86_64 build it needs Wayland and glibc 2.39+ (Ubuntu 24.04+, Fedora 40+, Arch). The same one-liner installs it, and quick support (`glassdesk.dev/help`) runs there too. Thanks [@Elshad](https://github.com/Elshad) for asking ([#101](https://github.com/Ntarasiuk/glass/issues/101)).

### Fit this device

- Fitting now gives your iPad or iPhone its full resolution: a Retina display in its shape (3180×2320 on a 12.9" iPad, laid out like 1590×1160), so text is sharp and a normal size.
- The fitted display stands in for the one you're watching: that monitor turns off while you're fitted, and the fitted display takes its place, menu bar and all. Before, the monitor mirrored it, which some monitors can't show (they lost the signal every few seconds and the screens flipped).
- Your other displays stay where they are, and come back exactly as they were when you stop.
- Turning your device resizes the fitted display to match, and windows that don't fit on it any more shrink and move onto it.
- The pointer reaches the very top of the fitted display, so the menu bar can be clicked.
- If Glass quits while fitted, macOS turns the monitor back on.

### Listen addresses

- `--bind` (or `glass config bind`) picks the addresses Glass listens on, for example `--bind tailscale --bind 127.0.0.1` to stay on your tailnet and this computer. `tailscale` means this machine's Tailscale addresses. The default is unchanged: every address. Also in the Mac app's settings. Thanks [@ddedic](https://github.com/ddedic).

## 0.10.0

October 31, 2026

### AI agents read Mac apps as text

- `snapshot` reads a Mac app's window (and its menu bar) as text: each button, field, menu item and line of text, with a ref on everything an agent can act on.
- `press` clicks a button, link, checkbox, tab, menu item or row by ref, through Accessibility. Your pointer doesn't move, even for a window behind others.
- `set_value` fills in a text field by ref without typing, and reports what the field shows afterwards.
- A ref is good only while its window stays the same: if it changed or closed, the action is refused ("Nothing was done") instead of hitting whatever is there now. Replies say whether the action happened, didn't, or was sent but not confirmed.
- The same rules as clicks: an agent's app limits and protected apps apply. Password fields are never read, and agents from other devices can't fill them in.
- Works with native and Electron apps, and with Chrome while its window is visible.

### Lock screen

- Typing the password at the lock screen from a viewer unlocks the Mac. Before, macOS rejected it as incorrect without checking it.
- Keys pressed into a password field aren't shown to viewers or written to the log ("a key, into a password field"). On Linux, the same while the session is locked (logind, hyprlock, swaylock and others).
- Agents with a grant can't type into password fields; they ask you instead.

### Stay awake with the lid closed

- New setting: `glass config lid-awake on`, or **Settings › Stay awake with the lid closed**. A closed laptop stays awake while it's plugged in, so you can still reach and unlock it. On battery it sleeps as usual.
- macOS: asks for the admin password once, to install a rule that allows only `pmset -a disablesleep 0|1`.
- Linux: no password; Glass holds systemd-logind's lid-switch inhibitor.

### Docs

- New section: [A locked screen, or a closed lid](https://glassdesk.dev/docs#a-locked-screen-or-a-closed-lid), and a changelog (this page).

## 0.9.1

October 31, 2026

- Faster Linux encoding: about 2× lower latency on 4K screens (parallel colour conversion, threaded OpenH264), and an optional VA-API hardware encoder.
- Home: pick the Tailscale or the from-anywhere address.
- Guides and comparisons: a redesigned, wider layout.

## 0.9.0

October 30, 2026

- Glass Fleet: every computer on your account in one list (details, actions confirmed with your passkey, and a policy that flags the ones breaking your rules). Off until you turn it on.
- Links reopen on the same device.
- A new home page.

## 0.8.3

October 29, 2026

- `glass status` shows whether the computer is ready to be reached from anywhere.

## 0.8.2

October 28, 2026

- Keyboard and mouse mode on touch screens: the device becomes a trackpad and keyboard while another screen shows the computer.
- Faster builds and CI.

## 0.8.1

October 27, 2026

- Capture restarts when a display wakes, so closing a MacBook's lid no longer freezes the picture.

## 0.8.0

October 26, 2026

- Your computers at glassdesk.dev/computers: passkey accounts (no email, no password) and `glass account add`.

## 0.7.0

October 24, 2026

- Trackpad mode and a compact toolbar on phones.

## 0.6.0

October 24, 2026

- Your device as a second display, Apple Pencil pressure and tilt, Glass Microphone, a file browser, rich clipboard (text, rich text, images), 120 fps and Display P3, and quick support.

## 0.5.0

October 24, 2026

- The Glass app for Mac, notifications on your devices, and Glass as an installable app everywhere.

## 0.4.1

October 23, 2026

- Docs cover display previews, the terminal and the toolbar.

## 0.4.0 and earlier

See [GitHub Releases](https://github.com/Ntarasiuk/glass/releases).
