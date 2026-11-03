# Changelog

What changed in each release of Glass. `glass update` (or the Glass app) installs the newest; every build is on [GitHub Releases](https://github.com/Ntarasiuk/glass/releases).

## Unreleased

### Tailscale not ready yet

- Your own tailnet devices connect without a token even when Tailscale isn't ready at the moment Glass starts (common right after signing in to Windows, or when the sign-in screen's Glass starts first). Glass keeps checking, and follows a later sign-in as someone else. A tagged device still asks for the token.

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
