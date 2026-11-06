# Glass

> Glass is a free, open-source, low-latency remote desktop for macOS and Linux (Wayland). Install it on the computer you want to control, then see and control it from a browser on an iPad, iPhone, or another computer. Typical latency is 15–30 ms from screen capture to display, over Tailscale or from anywhere through a one-time link (direct WebRTC, end-to-end encrypted). Nothing to install on the viewing device.

- Website: https://glassdesk.dev
- Install (on the computer to be controlled): `curl -fsSL glassdesk.dev | sh`
- Hosts: macOS 13+ (Apple Silicon and Intel), and Linux (x86_64 or ARM64) on Wayland with glibc 2.39+ (beta; built for Hyprland/Omarchy, should work on GNOME and KDE; e.g. Arch, Fedora 40+, Ubuntu 24.04+).
- Viewers: any modern browser with WebTransport and WebCodecs: Safari 26.4+ (iPad, iPhone, Mac), Chrome, Chromium, Edge and Firefox, on any OS including Linux.
- Price: free and open source (MIT): https://github.com/Ntarasiuk/glass

## What Glass does

Glass streams a Mac's screen to a web browser and sends mouse, keyboard, and touch input back. It is built for speed, using the same techniques as game-streaming tools (hardware video encoding, UDP-style transport, no buffering) instead of the video-call stack that tools like Chrome Remote Desktop use.

Features:

- **Low latency.** About 15–30 ms capture-to-display on a home network or Tailscale, measured live in the stats overlay.
- **120 fps and wide colour.** A viewer on a fast screen (120 Hz and up) gets up to 120 frames a second from a Mac whose display refreshes that fast; a wide-colour (Display P3) Mac display arrives in P3 instead of being clipped to sRGB.
- **Works in the browser.** Open a URL on the iPad; there's no app to install. Add it to the Home Screen for a true fullscreen app.
- **Full control.** Mouse, trackpad, keyboard, and touch (tap to click, drag, long-press to right-click, two-finger scroll), plus an on-screen keyboard button.
- **Fleet (new).** See every computer on your account in one list (model, system, disk, FileVault, firewall, battery), restart or update them from the page with a passkey confirmation, and mark the ones that break your rules. Off until you turn it on, computer by computer: [Glass Fleet](#glass-fleet-new).
- **Trackpad mode on phones.** The screen works like a laptop trackpad: drag to move the pointer, tap to click, two-finger tap to right-click; zoomed in, the view follows the pointer. The toolbar folds into menus on small screens.
- **Unlocks a locked Mac.** Type the password at the lock screen from the remote device, even on a MacBook with its lid closed (`glass config lid-awake on`, while it's plugged in).
- **Apple Pencil.** Pressure and tilt reach the Mac as tablet input, so drawing apps (Photoshop, Affinity, Krita) respond as they do to a drawing tablet.
- **Multiple displays.** Show one display, switch between them, or show all of them side by side in their macOS arrangement.
- **A second display.** Your iPad, a laptop or any browser becomes one more display that extends the Mac's desktop (Displays › Use as a second display), Retina where the screen is.
- **Audio.** Streams Mac system audio to the browser.
- **Clipboard sync.** Copy on the Mac, paste on the iPad, and the reverse: text, rich text and images.
- **File transfer.** Send files from the iPad to the Mac's Downloads folder, or pick files on the Mac and save them on the iPad. Transfers are direct and peer-to-peer.
- **A file browser.** The owner browses the computer's folders in the viewer (Files): previews, download, upload into any folder, new folder, rename, move to the Trash.
- **Auto-reconnect.** Survives iPad tab suspension, network changes, and host restarts.
- **Notifications.** On your phone, iPad or laptop, even with Glass closed: an agent needs you, someone asks to connect (Allow or Deny right from the notification), or a message. End-to-end encrypted, per device, and off until you turn them on.
- **A real app on every device.** Install it from the browser (Add to Home Screen on iPhone and iPad): its own icon with a badge, a shortcut to each of your machines, sharing photos and files to the Mac from any app, and it opens even when the computer is asleep to say so.
- **Self-updating client.** Open tabs reload when the Mac is updated.
- **Secure by default.** Devices on your own Tailscale tailnet are let in automatically; anything else needs an access token.
- **From anywhere.** `glass link --anywhere` makes a one-time link that connects any browser directly to the computer over WebRTC, end-to-end encrypted, with no ports opened. Off until you use it. A link lets in one device: if it opens in the wrong browser (an in-app browser, say), open it again in the right one on the same device within 15 minutes. Up to three browsers can use it, from the same network and the same kind of device, and the access still ends when the first one's does. A passkey link stays with the first device.
- **One-time links.** Let another device in without sharing the token: each link works once and gives that device its own pass, revocable with `glass link --revoke`.
- **Links with limits.** View-only, no files, no clipboard, a time limit, ask the person at the computer first, or require a passkey (Face ID, Touch ID or a security key) every time.
- **Helping someone.** Chat, drawing on the screen (shown on the computer too), a privacy screen that blanks the computer and blocks its own keyboard and mouse, and a connection history.
- **Quick support.** Someone without Glass pastes one line (`curl -fsSL glassdesk.dev/help | sh`) and reads you a code; you type it at glassdesk.dev/help and connect once they allow it. Nothing stays installed afterwards.
- **Sharp text.** A per-viewer mode for code and documents: native size, finer frames, HEVC 4:2:2 colour while things move and a lossless full-colour (4:4:4) picture once the screen is still.
- **One window.** Stream a single app's window instead of the whole display (macOS).
- **Terminal and ports.** A shell on the computer inside the viewer, and its local web servers (`/fwd/3000/`) opened from your tailnet.
- **Recording.** Record sessions to MP4 on the computer, per session or always.
- **Microphone and controllers.** Your microphone reaches the computer, so its apps hear you (on a Mac through Glass Microphone, a small audio driver Glass installs with one password prompt). Game controllers reach Linux hosts, and Windows hosts with the free ViGEmBus driver.
- **Keyboards per machine.** ⌘↔Ctrl, PC-keyboard and ⌘/⌥ swaps are remembered for each computer; an optional shortcut bar puts Mac shortcuts one tap away.
- **Power.** Wake a sleeping machine (Wake-on-LAN), restart Glass or the whole computer and reconnect.
- **AI agents, supervised.** `glass mcp` lets Claude Code, Claude Desktop, Cursor or any MCP client use the computer (and your other computers running Glass) while you watch each step, pause or stop it, and approve anything risky, from the viewer, the Mac app or a notification. On a Mac it can also read an app's window as text (`snapshot`) and press buttons or fill in fields by reference (`press`, `set_value`), without moving your pointer.
- **The Glass app (macOS and Windows).** A native window for everything the command line does: setup, who's connected, links with QR codes, machines, history, AI agents, settings and teams. On a Mac in Applications, Spotlight and the menu bar (**Open Glass…**); on Windows in the Start menu and the tray icon.
- **A console.** `https://<computer>/admin/`: who's connected, requests to allow, links with QR codes, devices, history, settings, access rules and an address book of your machines.
- **For teams.** Single sign-on (any OpenID Connect provider), LDAP, SCIM provisioning, access rules per email, domain or group, and device posture checks.
- **Self-hostable relay.** Run the remote-access introducer and TURN relay yourself.

## Install

On the Mac you want to control, run in Terminal:

```sh
curl -fsSL glassdesk.dev | sh
```

The installer:

1. Checks that this is macOS 13 or newer.
2. Downloads the latest release (a universal binary for Apple Silicon and Intel) and verifies its SHA-256 checksum.
3. Installs to `~/.glass` and links the `glass` command into `~/.local/bin` (adding that to your PATH if needed).
4. Adds **Glass.app** to `~/Applications` (macOS) and opens it, where setup takes a few clicks. Over SSH, or on Linux, it starts `glass setup` in the terminal instead.

Running the same command again updates Glass in place (it says which version it updated from), restarts the background service, and skips the setup guide if Glass is already set up. Settings and macOS permissions carry over.

## The Glass app (macOS and Windows)

Open **Glass** from Applications or Spotlight, or choose **Open Glass…** from the menu bar icon. On Windows, open **Glass** from the Start menu or the tray icon. It does everything the `glass` command does, in a window:

- **Home:** whether Glass is ready, the address to open with a QR code to scan, anything that needs fixing (a permission, Tailscale), and the switches used most.
- **Connected:** who's viewing (end any session), connection requests to allow or deny, the chat with viewers (both sides, and reply), the privacy screen.
- **Links:** one-time links with limits, shown as a QR code, and the devices they let in, to revoke.
- **Machines:** your computers on Tailscale, to open or wake.
- **History:** every session and refused attempt.
- **AI agents:** add Glass to Claude Code, Claude Desktop or Cursor in one click; pause or stop an agent; answer its questions.
- **Settings:** every `glass config` setting, the access token, restarting or turning off the service, updates and feedback.
- **Teams & SSO:** single sign-on, access rules, LDAP, SCIM and device posture.

The first time it opens, it walks through setup. It's a separate process from the background service, drawn on the GPU (with [GPUI](https://www.gpui.rs)), and quits when you close its window; Glass keeps running in the background.

## Setup (`glass setup`)

The setup guide (in the app, or `glass setup` in a terminal) walks through four steps and checks each one live:

1. **Tailscale (remote access).** Glass uses [Tailscale](https://tailscale.com) to reach the Mac securely from other devices, with no port forwarding. Setup checks that Tailscale is installed and signed in, and that **HTTPS Certificates** are enabled for your tailnet (one switch on https://login.tailscale.com/admin/dns; browsers require HTTPS for the APIs Glass uses). It then fetches a certificate for the Mac's tailnet name. Without Tailscale, skip this step: setup ends by offering a one-time link that connects your iPad from any network instead (see *From anywhere* below).
2. **Background service.** Glass installs a per-user LaunchAgent (`dev.glassdesk.host`) so it runs in the background, optionally starts at login, and restarts if it crashes.
3. **Screen Recording permission.** macOS asks to let `glass` record the screen. Setup opens the right System Settings pane (Privacy & Security › Screen Recording on macOS 13–14, Screen & System Audio Recording on 15+) and waits until capture works.
4. **Accessibility permission.** Needed to inject mouse and keyboard input. Setup opens the pane and waits until it's allowed.

It finishes by showing the connect URL and a QR code to scan with the iPad.

## Let an AI agent use your computer

`glass mcp` is an [MCP](https://modelcontextprotocol.io) server that gives an AI agent your computers running Glass: their screens, pointers and keyboards. One server reaches all of them: `list_machines` shows the computers on your tailnet, and every tool takes a `machine` (by name) to work on one, so "on the Mac mini, …" just works. You watch what it does live from any viewer, and stay in charge:

- A banner shows the agent at work and its recent steps, and a ring marks every click. **Pause** holds its next step until you resume; **Stop** refuses everything until you **Allow again**.
- Before anything hard to undo (sending, buying, deleting, posting) the agent asks you, and a card in the viewer lets you **Allow** or say **Don't**, with an optional note.
- When it's stuck (a sign-in, a 2FA code, a CAPTCHA, a choice only you can make), it asks for help: you do that part from the viewer, tap **Done, hand back**, and it carries on. You also get a notification on the computer.

Add it to Claude Code:

```sh
claude mcp add -s user glass -- glass mcp      # this computer, and the others by name
```

To make another computer the default instead of this one: `glass mcp --machine mac-mini`. Other computers need Glass 0.4.2 or newer, and accept an agent only from their owner's devices (signed into Tailscale as the same user).

Then ask, for example: "on my Mac, export the frames from the open Figma file to the Desktop". Any MCP client works the same way (command `glass`, argument `mcp`).

Tools: `list_machines`, `screenshot`, `list_displays`, `click` (left, right, middle; double and triple), `move_pointer`, `drag`, `scroll`, `type_text` (any text), `press_keys` (`cmd+s`, `return`, `ctrl+c`…), `wait_for_change`, `snapshot`, `press`, `set_value`, `request_help`, `confirm` and `check_answer`. `confirm` and `request_help` take an optional `wait_seconds`: when you haven't answered by then, they say the question is pending (it stays up for you), and `check_answer` collects your answer later. Coordinates are in the pixels of the latest screenshot (1280×800 at most unless the agent asks for more), so they work with computer-use models as is.

`wait_for_change` follows what the computer's capture says changed (ScreenCaptureKit, Desktop Duplication or PipeWire), so even a blinking caret or a spinner counts. By default it returns once the screen has been still for `idle_ms` (300 ms); with `change: true` it waits for a change first. Screenshots come with a `seq`: passed as `since`, only changes after that picture count, even ones that happened before the wait began. `region` (`[x, y, width, height]` in screenshot pixels) limits it to part of the screen.

`record` makes an MP4 of the screen from the video Glass already sends viewers (nothing is re-encoded): for a few seconds (60 at most), or started and stopped by id (10 minutes at most). The agent gets the video back when it's 20 MB or less, and the file stays on the computer for a day, in Glass's config folder under `agent-recordings`. You see it in the viewer when an agent starts and stops recording. An agent you connected with a grant has its recording deleted if a protected app (passwords, keychains, or an app outside its limit) comes to the front.

On a Mac, `snapshot` reads an app's window (and its menu bar) through Accessibility as text: each element's role, name and value, with a ref like `@k3f9:e12` on each one it can act on. `press` (buttons, links, checkboxes, tabs, menu items, rows) and `set_value` (text fields) act on a ref without moving your pointer, even in a window behind others. A ref is good only for the snapshot it came from: if the window changed, nothing happens and the agent is told to take a new snapshot. Password fields never show their contents, and an agent under a grant can't fill one in. Chrome and Electron apps (Slack, VS Code…) show a page's content only while their window is visible, so the agent brings the window to the front first; an app that has just launched may need a moment before it answers. Canvases, games and video don't appear in a snapshot: the agent uses screenshots for those.

Only the computer's owner can drive it: your own tailnet devices, or this computer's token (which `glass mcp` reads by itself on this computer). Guests and links can't. Under the hood it's an HTTP API at `https://<computer>/agent/v1/` (`POST /agent/v1/act` with `{"action": "click", "x": 640, "y": 400}` and so on, sent as `Content-Type: application/json`), if you'd rather call it directly.

## Agents from anywhere

Agents that don't run on your computers (on Vercel, in claude.ai or ChatGPT, on a server) can ask to use one. Glass is a standard MCP server over HTTP with OAuth sign-in, so anything that speaks the [MCP authorization spec](https://modelcontextprotocol.io/specification/2025-11-25/basic/authorization) works, including [Vercel Connect](https://vercel.com/docs/connect):

```sh
glass agents public on       # publish it with Tailscale Funnel; prints the URL
vercel connect create https://mac-mini.tail1234.ts.net:8443/mcp
```

On your tailnet no publishing is needed: use `https://<computer>/mcp`. Publishing needs Funnel allowed for the machine in your tailnet's settings; if it isn't, `glass agents public on` prints the link to allow it.

**Approving takes one tap.** The agent sends you to a sign-in page on your computer. Opened through your tailnet address on one of your other devices (signed in to Tailscale as you), it has an **Approve** button, which works a moment after the page appears. Anywhere else (through the Funnel address, on the computer itself, behind `tailscale serve` on Linux) it shows a number, and Glass asks you to tap the matching one of four, in the viewer or the Mac app; the notification tells you where. Picking the wrong one denies it, and that agent can't ask again for 10 minutes, so guessing doesn't work. The computer never approves on its own: not from a browser on its own screen, and not from its own viewer while an agent is working; nor while a guest is connected. After that, the agent works with no prompts: its access refreshes itself, and a grant ends after 30 days unused (180 days at most).

**What an agent can do is checked on your computer, at every step:**

- `computer:view` (screenshots) and `computer:use` (pointer and keyboard). A view-only agent that asks to click gets told to ask you for more.
- It never drives password managers (Passwords, 1Password, Bitwarden…), Keychain Access, System Settings, the system's password prompts, terminals, Screen Sharing or Glass itself, and never types into a password field. It's told to ask you instead. Glass checks the app that would actually get the input (what's under the pointer, or what has the keyboard, Spotlight and Control Center included), not just the app in front.
- An agent can limit itself to some apps (`authorization_details` of type `glass_computer` with `apps`); input anywhere else (other apps, Spotlight, the Dock) is refused.
- It doesn't see protected apps: screenshots are refused while one (or, for an agent limited to some apps, any other app) is in front, and so are snapshots of them.
- When you use the computer, at its own keyboard and mouse or by clicking, typing or scrolling in a viewer, agents wait until you've stopped for a few seconds (an agent from another device is told to try again after 20 seconds).
- 1,500 actions an hour per agent; an agent over it is held until the hour ends (you get a notification), or until you resume agents.
- On Linux, Glass can't tell which app input reaches, so it can't keep an agent out of particular apps or password fields (the sign-in page says so), and agents can't limit themselves to apps there.
- Pause and Stop in the viewer and the Mac app work as for any agent; the banner shows its name. What it types isn't shown or logged.

**In the Mac app** (AI agents): turn agents on or off, copy the MCP address, publish with Funnel, install apps that asked, see and revoke agents, and see what they did lately.

**Revoking is immediate**: in the Mac app, with `glass agents revoke <id>` (or `all`), or from the agent's platform (Vercel's revoke calls Glass's). An old refresh token used again (a stolen copy) revokes its grant on the spot. Approving an agent again replaces its earlier grant. **Turning it off:** the Mac app's Settings (AI agents), the console's settings, or `glass config agents off` (the same as `glass agents off`) refuses every agent from another device and stops publishing with Funnel. Grants are kept for when it's back on. Agents on the computer itself (`glass mcp`) aren't affected.

**Every action is logged** to `agent-audit.jsonl`, each line chained to the one before by its hash: `glass agents log` shows the latest, `glass agents verify` checks nothing was changed.

**Cloud agents never hang on you.** A question (`confirm`, `request_help`) holds the agent's request 20 seconds at most (it can ask for up to 50), then tells it the question is pending, so serverless functions don't time out. The question stays up in Glass and on your phone for 30 minutes; the agent does other work and collects your answer with `check_answer`. It's told not to take the action until it's approved.

**Unattended agents** (Vercel Connect's `app` subject, OAuth client credentials, by dynamic registration or a client ID metadata document) need installing once on the computer: when one asks, you get one notification, and `glass agents` shows `glass agents install <client>`. Revoking an app's grant uninstalls it.

For providers: authorization server metadata at `/.well-known/oauth-authorization-server`, protected resource metadata at `/.well-known/oauth-protected-resource/mcp`, client ID metadata documents or dynamic registration (RFC 7591/7592), PKCE S256, `authorization_code`, `refresh_token` and `client_credentials`, client authentication `none`, `client_secret_basic`/`post` or `private_key_jwt`, revocation (RFC 7009), and the `iss` parameter on redirects (RFC 9207). Access tokens last 10 minutes. With Funnel on, only these routes are published; the viewer and everything else stay on your tailnet.

## Set up with an AI agent

Glass ships an agent skill: https://glassdesk.dev/skill.md. With it, an agent like Claude Code can set Glass up on any Mac or Linux machine in your Tailscale tailnet over SSH. It finds the machine, installs Glass, runs the unattended setup, tells you exactly which permission switches to click on that machine's screen, waits until everything works, and verifies the connection.

Install it for Claude Code:

```sh
mkdir -p ~/.claude/skills/glass && curl -fsSL https://glassdesk.dev/skill.md -o ~/.claude/skills/glass/SKILL.md
```

Then ask, for example: "set up Glass on my Mac mini". The target needs SSH: Tailscale SSH (`sudo tailscale set --ssh`) or Remote Login on macOS.

The skill drives two commands you can also use in your own scripts:

- `glass setup --yes --json`: does every setup step that needs no human (using `sudo` only if it needs no password), opens the macOS permission panes on the machine's screen, and prints a report: `ok`, `url`, `running`, `capturing`, `input`, and `needs`, the steps left, each with an `action` and whether it must happen `on_screen`.
- `glass status --json`: whether the service is running, capturing, and allowed to inject input, and whether it's ready to be reached from anywhere (`remote_ready`, with `remote_problems` saying why not).

## Connecting

1. Install Tailscale on the iPad (or other device) and sign in with the same account as the Mac.
2. Open `https://<your-mac>.<your-tailnet>.ts.net` (shown at the end of setup, and by `glass status`). The same address with `:8080` also works.
3. Optional: in Safari or Chrome, tap **Share → Add to Home Screen**, then launch Glass from its icon. It runs fullscreen with no browser UI, so Safari's own shortcuts (⌘W, ⌘T, ⌘L, ⌘R) reach the Mac instead.

Devices that aren't on your tailnet, or belong to another tailnet user, are asked for the access token (`glass token` shows it; `glass token --set` lets you choose one). A link containing `#t=<token>` signs a browser in once and is then remembered.

**Magic links.** To let in a device without handing out the token, run `glass link` on the host; it prints the link and a QR code. The link works once, within 15 minutes, and gives that device its own access for 24 hours by default (`glass link --for 7d` to change it). Guests can use the computer but can't switch machines or update Glass. `glass link --revoke` cuts every guest off within seconds and voids unused links; your token and tailnet devices are unaffected. Only fingerprints of links and passes are stored, in `~/.config/glass/access.json`. On your tailnet, the guest's device must be on the tailnet too; for anywhere else, see the next paragraph.

**From anywhere (no Tailscale on the device).** `glass link --anywhere` makes a link like `https://glassdesk.dev/app/#r=…&l=…` that works from any network. The browser and your computer connect directly over WebRTC: UDP, end-to-end encrypted, about as fast as on the tailnet. When a direct path is impossible (some cellular and corporate networks), traffic goes through Cloudflare's TURN relay, still end-to-end encrypted. glassdesk.dev only introduces the two sides; your computer keeps one outgoing connection to it and opens no ports. The link carries your computer's public key and the computer signs every connection answer, so the browser refuses anyone else, glassdesk.dev included. Remote access is off until you make such a link (or run `glass remote on`); `glass remote off` turns it off. One thing to know: in this mode the viewer page itself comes from glassdesk.dev, whereas on your tailnet it comes from your own computer.

**Your computers (glassdesk.dev/computers).** Sign in there with a passkey (no email, no password; it syncs through iCloud Keychain, Google Password Manager or your password manager) to see which of your computers are online and open any of them from any device. To add a computer, run `glass account add` on it (remote access must be on) and open the one-time link, or scan its QR code, where you're signed in. Your computer signs that claim with its key, so nobody can add a computer they don't control. The account only lists computers: opening one still takes that computer's own sign-in, so someone who got into your account would see names and who's online, never a screen. `glass account remove` takes a computer off every account; deleting the account on the page deletes its list. glassdesk.dev keeps the passkey's public key, each computer's name and public key, and a hash of each sign-in cookie. To look after your computers rather than just reach them (their state, actions you can take, rules they should meet), see [Glass Fleet](#glass-fleet-new).

## Help someone (quick support)

For helping a parent or a friend who doesn't have Glass, and shouldn't have to install anything:

1. They open Terminal (on a Mac: Spotlight, type Terminal) and paste `curl -fsSL glassdesk.dev/help | sh`. If they already have Glass, `glass support` does the same.
2. A Glass window shows a nine-digit code (in the terminal on Linux or over SSH). The first time, macOS asks them to allow Screen Recording and Accessibility for Glass; the window has buttons for both.
3. You type the code at [glassdesk.dev/help](https://glassdesk.dev/help). They're asked before you can see anything, and your viewer says "You're helping …".
4. They click **Stop sharing** (or press Ctrl+C, or close the window) when you're done.

Nothing stays on their computer: Glass runs from a temporary folder with its own temporary settings, token and keys, not as a background service and not at login, and stopping deletes all of it. The connection is the same as `glass link --anywhere`: direct, end-to-end encrypted, no ports opened. Each code works once, for an hour; glassdesk.dev keeps only a salted hash of it, and a code typed wrong too often is locked out for that browser. **New code** in the window (or `n` in the terminal) replaces it. A downloadable app for the same thing needs Apple notarization and is coming; for now it's the one line.

## Glass Fleet (new)

**Fleet is a new set of things in Glass, separate from connecting to a computer.** Until now Glass got you *into* a computer. Fleet is for looking *after* the computers on your account: seeing what state they're in, doing things to them without opening a screen, and checking them against rules you set. It needs Glass 0.9 or newer on each computer, and everything in it is **off until you turn it on, computer by computer**. If you never use it, nothing else in Glass changes.

| | What it is | Turn it on | What authorizes it |
|---|---|---|---|
| **Details** | Each computer tells your account its model, system, disk, encryption, firewall, battery and more, so you can see them all in one list. | `glass fleet on` | The computer signs each report with its own key. |
| **Actions** | From your account's page: refresh details, update Glass, restart Glass, restart the computer, or run a script you placed there. | `glass fleet manage` | Your passkey signs each action, and the computer checks it. |
| **Policy** | Rules every computer should meet (FileVault on, a minimum macOS…). The page marks the ones that don't. | Set it on the page. | Nothing: it only reads the details. It can't change a computer. |

### Set it up

1. Update Glass to 0.9 or newer on the computer, and turn on remote access: `glass remote on`.
2. Put the computer on your account: `glass account add`, then open the link where you're signed in at [glassdesk.dev/computers](https://glassdesk.dev/computers).
3. For **details**: `glass fleet on`. It sends the first report straight away, and the details appear on the computer's row. (If it says "not sent yet", the computer isn't on an account: do step 2.)
4. For **actions**: `glass fleet manage` on the computer prints a link and a QR code. Open it where you're signed in, press **Allow** and confirm with your passkey. The computer's row gets an **Actions** button.
5. For **policy**: open **Policy** on the page and tick the rules you want.

### Details

Every 15 minutes (and when you ask), a computer on your account sends this, and only this:

- Model and chip (`MacBook Pro`, `Apple M3 Max`), and the system (`macOS 15.1`, or the Linux distribution's name).
- Memory, disk size and free disk space.
- Whether disk encryption (FileVault) and the firewall are on. A computer that can't say (FileVault doesn't exist on Linux) leaves it out.
- Uptime, battery percentage (when there is a battery), the Glass version, and whether actions are set up on it.

The computer signs each report with its own key. glassdesk.dev keeps only each computer's latest report, and only for computers that are on an account. The page shows it under the computer's name and marks what needs attention: FileVault or firewall off, a nearly full disk, a low battery, a report that has gone stale. With four or more computers there's a search box that matches names, status and these details.

`glass fleet status` shows whether it's on and when it last sent. `glass fleet report` sends one now and prints it. `glass fleet off` stops reporting **and deletes what was sent**; so does `glass account remove`.

### Actions

An action is something your account asks a computer to do. The list is fixed:

| Action | What happens |
|---|---|
| Refresh details | The computer sends a report now. |
| Update Glass | The computer updates itself and restarts. |
| Restart Glass | Glass restarts and anyone viewing reconnects in a few seconds. |
| Restart computer | The computer restarts (open apps are asked to quit); Glass is back once someone is logged in. |
| Run a script | Runs a script you placed on the computer, by name. |

**Scripts.** Put them in `~/.config/glass/fleet/scripts/`. Glass only runs a script when the folder and the file are both yours and nobody else can write them (`chmod go-w`), the file is executable and a plain file (not a link), and its name is letters, digits, dots, dashes and underscores. It runs for at most a minute (and is stopped, along with anything it started, after that), and the last 2,000 characters of its output come back to the page. Glass never runs anything you didn't put there.

**How an action is authorized.** Each time, the page asks for your passkey, so you confirm with your fingerprint, face or PIN, and builds a command: this computer, this action, a one-time number, an expiry a couple of minutes away. The passkey signs that exact command. glassdesk.dev passes it to the computer, and **the computer checks the signature itself** against the passkey it saved when you set it up. It refuses a command that is expired, was already run, is for another computer, was signed for another site, wasn't confirmed by you, or whose passkey it doesn't know. The page has no way to run anything the computer hasn't checked.

**Setting up a passkey (`glass fleet manage`).** The link holds a one-time code that never goes to glassdesk.dev, because it sits after the `#`, which browsers don't send. The page uses that code to vouch for your passkey's public key, and the computer saves the key only if the vouch checks out. The link works once, for 15 minutes, for one passkey. Run it again to add another passkey (up to 8). Only ES256 passkeys can manage computers, which covers nearly all phones, Macs and security keys.

Every action, and every refusal, is logged on the computer: `glass fleet manage status` shows the latest, and the full log is `~/.config/glass/fleet/actions.log`. `glass fleet manage off` forgets every passkey and stops all actions at once.

### Policy

The rules live under **Policy** on the page, stored with your account:

- Disk encryption (FileVault) on.
- Firewall on.
- Glass up to date (the newest release).
- At least a set amount of free disk space.
- At least a set macOS version.
- Details no older than a set number of hours.

Each computer's latest details are checked against the rules in the page. A computer that breaks one says so under its name (`Not compliant: firewall off, system older than 16`), and the count of computers that meet the policy sits next to **Policy**. A check that can't be made (FileVault on Linux) isn't failed, and a computer that hasn't reported yet isn't judged. Setting a rule changes nothing on any computer; to fix what it finds, use an action.

### What can and can't happen

| | |
|---|---|
| **glassdesk.dev can** | Store each computer's latest details; store your policy; relay a signed command to a computer that's online. |
| **glassdesk.dev can't** | Run an action by itself, forge a command (it has no passkey), keep an action's result (it passes through to the page and isn't stored), or see a screen: opening a computer still takes that computer's own sign-in. |
| **Someone who steals your signed-in browser** | Can see names, status and details, but can't run an action: each one needs your passkey, confirmed with your fingerprint, face or PIN. They could add a passkey of their own to the account, but a computer only takes commands from a passkey you pinned with a link printed on the computer itself. |
| **The one moment of trust** | Pinning happens on the page glassdesk.dev serves. After that, the website alone can't act. |

Treat the `glass fleet manage` link like a password for 15 minutes: don't paste it into a shared chat or an agent's transcript.

### Commands

```
glass fleet                      Is reporting on, and when was the last report?
glass fleet on | off             Start reporting; stop and delete what was sent
glass fleet report [--json]      Send a report now
glass fleet manage [--json]      Link to let your account run actions here (also turns reporting on)
glass fleet manage status        Passkeys that can run actions, and the latest actions
glass fleet manage off           Forget every passkey; stop all actions
```

### Not included

- **Teams and roles.** An account is one passkey identity (up to 20 passkeys), so a team shares an account; there are no per-person permissions yet.
- **Zero-touch setup and an MDM.** Fleet doesn't enroll computers through Apple Business Manager or install profiles; use your MDM to install Glass, then Fleet to look after it.
- **Automatic fixes.** A policy only flags; an action fixes, when you press it.
- **Offline computers.** An action needs the computer to be online. A computer that's off shows its last details with their age.

### If something's wrong

- **No Actions button.** The computer must be online and have details on (`glass fleet on`) and a passkey saved (`glass fleet manage`). It appears after its next report, within a few seconds of saving the passkey.
- **"this computer isn't set up for actions".** No passkey is saved here: run `glass fleet manage` and open the link.
- **"that command has expired".** The computer's clock is off by minutes: fix its date and time.
- **"that passkey isn't pinned on this computer".** You confirmed with a different passkey than the one you saved. Run `glass fleet manage` again while using that passkey.
- **"no enrollment is open".** The link was used or is older than 15 minutes: run `glass fleet manage` again.
- **"This computer isn't on an account".** Add it first: `glass account add`.

## Links with limits

`glass link` takes options that limit what the device may do:

- `--view-only`: watch only; no mouse, keyboard or controllers.
- `--no-files`, `--no-clipboard`: no file transfer, no copy and paste.
- `--for 1h`: how long the device's pass lasts (default 24 hours).
- `--ask`: the person at the computer allows each connection, from a notification or with `glass allow` / `glass deny`. `glass config ask-guests on` asks for every guest; `ask-everyone on` asks for everyone.
- `--passkey`: the device registers a passkey on first use and must use it every time, so a copied link is useless on another device.

`glass link --list` shows the devices let in by links, each with an id; `glass link --revoke <id>` cuts off one. `glass viewers` shows who's connected now and `glass kick [id]` ends a session. `glass log` is the connection history: who, when, how, and for how long.

## Using Glass

The toolbar (top right) groups its buttons: machines and displays, keyboard, sound, files, things for everyone connected (chat, drawing, recording), tools (terminal, ports), and the view (this device, fullscreen, the **…** menu, hide). It has:

- **Displays:** pick one display, or **All displays** to show every screen side by side in the same arrangement as System Settings → Displays.
- **⌘ (Mac shortcuts):** one-tap app switcher, Spotlight, Mission Control, Hide, Quit, Close window, screenshot, Force Quit, and Lock Screen. Also keyboard remapping options (see below).
- **Keyboard:** brings up the iPad's on-screen keyboard.
- **Send files to Mac:** uploads to `~/Downloads` (drag and drop also works, folders included).
- **Get files from Mac:** opens a file chooser on the Mac's screen; chosen files arrive with a Save button.
- **Files** (the computer's owner only): browse the home folder from the viewer, with previews of images, PDFs and videos. Filter as you type, download (or drag a file onto your desktop in Chrome), upload into the folder you're in (button, or drop files and folders on it), make folders, rename (F2) and move to the Trash (Delete). Arrow keys, Enter to open, Backspace to go up.
- **Audio:** turns Mac system audio on or off for this device. Your browser's media controls (Chrome's media hub, the iPhone or iPad lock screen, media keys) show what the computer is playing, with its artwork, and play, pause, skip and seek it there.
- **Fullscreen** (hidden when running as a Home Screen app).
- **… (More):** **Stats** (latency, frame rate, bitrate; Ctrl+Shift+H) for everyone, and for the computer's owner the privacy screen, restarts and the console.
- **Hide toolbar** (last button): slides it away for an unobstructed screen; the small tab at the top brings it back. Remembered.

**Display previews.** With two or more displays, a strip of small live previews of each one (and **All**) sits in the empty band below the picture whenever there's room; the one being shown is outlined, and tapping another switches to it. Without room it waits behind a small tab at the bottom. Hide it with its ⌄ button; that's remembered. Live previews of displays not being shown need macOS 14 or newer.

Touch gestures: tap = click, drag = drag, long-press = right-click, two fingers = scroll, pinch = zoom.

**Trackpad mode** (the trackpad button, in the keyboard group; on by default on phones): the screen works like a laptop trackpad. Drag to move the pointer (faster flicks go further), tap to click, two-finger tap to right-click, double-tap and hold (or press and hold) to drag, two fingers to scroll, pinch to zoom; when zoomed in, the view follows the pointer.

**Keyboard and mouse mode** (touch screens: ⋯ menu › Keyboard and mouse mode): for when another screen shows the computer, such as a car's browser or a TV, and this phone is only its trackpad and keyboard. The picture is paused and hidden here (the computer stops sending it, and this device never raises the picture's size for others), the whole screen is the trackpad, and the toolbar's keyboard types. Tap **Show the screen** to go back.

On a phone the toolbar folds each group into one button with a dropdown.

Apple Pencil and other styluses draw with pressure and tilt: the Mac sees a pen tablet, so Photoshop, Affinity, Krita and other drawing apps vary the stroke. Hovering (Pencil hover on newer iPads) moves the pointer without clicking.

More in the toolbar:

- **Displays › Sharp text:** for code and documents. See *Sharp text* below.
- **Displays › Apps:** show one app's window on its own (macOS).
- **Displays › Use as a second display** (macOS, owner): this screen becomes one more display of the Mac's, to the right of its main one (Retina on an iPad), so the desktop extends onto it like Sidecar, from any browser. Drag windows onto it from the Mac; turn it off to go back to what you were watching. It goes away when you disconnect. Everyone viewing sees the same display, so it's best used alone.
- **Draw on the screen:** annotate; the drawing also shows on the computer's screen.
- **Chat:** messages with the person at the computer (`glass chat "…"` from there).
- **Terminal:** a shell on the computer, in the viewer. Drag its top edge to resize it, or use **Full screen** (or double-click its title bar); both are remembered.
- **Forwarded ports:** open the computer's local web servers (only ports allowed with `glass config forward-ports 3000,8888`, and only for your own devices).
- **Record on the host:** records this session to `~/Movies/Glass` (`~/Videos/Glass` on Linux). `glass config record on` records every session.
- **Microphone:** sends your microphone to the computer, where apps pick it like any other mic.
  - **Mac:** it comes in as **Glass Microphone**, a small audio driver included with Glass, made the Mac's default input while your mic is on and put back after. Install it once: tap the mic in the viewer (owner), the Glass app (Home or Settings › Microphone from your devices), or `glass mic install`; it asks for the Mac's password. BlackHole works too if it's already installed. `glass mic uninstall` removes the driver.
  - **Linux:** it comes in as a "Glass microphone" input while your mic is on, through PipeWire or PulseAudio. It needs `pactl`; `glass mic install` adds it with your package manager if it's missing.
- **… (More), for the owner:** **Privacy screen** (blanks the computer's screen and ignores its keyboard and mouse while you're connected; `glass privacy on|off` from there), **Restart Glass**, **Restart computer** (Glass reconnects once someone is logged in) and **Console**.
- **Machines:** sleeping machines Glass has seen before can be woken with Wake-on-LAN (also `glass wake <name|mac>`).
- **This device** (the bell): what this browser does for Glass.
  - **Notifications:** messages, questions (an agent asking before it acts or needing your help, someone asking to connect) and activity (someone joined, an update is ready), even with Glass closed. On iPhone and iPad, add Glass to the Home Screen first. `glass notifications` lists the devices that get them (`test`, `off <id>`), as does the Glass app's Settings.
  - **Smooth motion:** on a screen faster than 60 Hz, up to 120 frames a second (on by default there; macOS hosts, when the Mac's display is that fast too). Every viewer shares the stream, so with a 60 Hz viewer connected everyone gets 60. The stats overlay shows the frame rate. Browsers that hold pages to 60 Hz (Safari, by default) stay at 60.
  - **Keep screen awake** while connected (on by default).
  - **Picture in picture:** the screen in a floating window while you do something else.
  - **Use all my screens:** with several monitors here and several displays there, one display on each (Chrome and Edge).
  - **Capture mouse:** for games and 3D apps, the pointer stays in the picture and sends relative motion; Esc releases it.
  - **Sync clipboard:** switching back to Glass puts this device's clipboard on the computer. Chrome and Edge do it by themselves; Safari and Firefox show a Send button, then their own Paste button to confirm.
  - **Pause while away:** stops the video while this computer is locked or idle (Chrome and Edge).
  - **Install Glass:** its own window and icon. On a desktop, the toolbar moves into the title bar.

**As an installed app** (Home Screen, or **Install Glass**): long-press the icon for Chat, Machines, Send files and each of your other machines; share photos, files or text to Glass from other apps (Android, ChromeOS, desktop) to send them to the computer; open files with Glass on the desktop; `web+glass://<machine>` links open that machine. It opens even when the computer can't be reached, and says so. Only one tab of a browser shows a computer at a time; another tab asks before taking over.

Game controllers connected to the viewing device work on Linux hosts, and on Windows hosts with the free [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) driver installed, as a virtual Xbox 360 controller.

Local shortcuts (not sent to the Mac): Ctrl+Shift+F toggles fullscreen, Ctrl+Shift+H toggles the stats overlay.

### A locked screen, or a closed lid

Type your password at the lock screen from the viewer, as you would at the computer. Keys pressed into a password field (the lock screen, a password prompt) aren't shown to other viewers or written to Glass's log: they see "a key, into a password field". Agents with a grant can't type into one at all; they ask you with `request_help`.

A laptop sleeps when its lid closes, and a sleeping computer can't be unlocked from anywhere. Turn on **Stay awake with the lid closed** (Glass app › Settings, or `glass config lid-awake on`) to keep it awake with the lid closed while it's plugged in. On battery it sleeps as usual, so it still sleeps closed in a bag. Keep a closed laptop that stays awake somewhere it can cool down.

- **macOS:** turning it on asks for your admin password once, to install `/etc/sudoers.d/glass-lid`. That rule lets Glass run exactly `pmset -a disablesleep 1` and `pmset -a disablesleep 0` and nothing else. Glass only turns back on a sleep it turned off itself. A virtual display (as Glass makes for **Use as a second display**) doesn't keep a closed Mac awake; a real external display does. If your company manages the Mac, its management software may remove the rule; turn the setting on again to reinstall it.
- **Linux:** no password. While plugged in, Glass holds systemd-logind's `handle-lid-switch` inhibitor, if polkit allows it (it usually does for your own session). GNOME and KDE decide about the lid themselves, so set their own lid action too; Hyprland, Sway and others leave it to logind.

### Keyboard and ⌘ shortcuts on iPad

iPadOS reserves a few shortcuts before any app or web page sees them: ⌘Tab, ⌘Space, ⌘H, ⌘⇧3/4/5, and Globe-key combinations. Glass offers three ways around this:

- **Ctrl as ⌘** (on by default): Ctrl+Tab, Ctrl+Space, Ctrl+H, and Ctrl+⇧3/4/5 are sent to the Mac as their ⌘ versions. Holding Ctrl and tapping Tab cycles apps exactly like ⌘Tab. Every other Ctrl combination stays Ctrl.
- **Swap ⌘ and ⌥:** pair with iPad Settings → General → Keyboard → Hardware Keyboard → Modifier Keys (Command → Option, Option → Command). The physical ⌘ key then reaches the Mac as ⌘ even for reserved shortcuts.
- **⌘ menu:** one-tap buttons for the reserved shortcuts.

These choices are remembered for each computer. **Shortcut bar** (⌘ menu) adds a row of Mac shortcuts above the screen on touch devices.

Everything else, including ⌘C, ⌘V, ⌘Q, ⌘W, and arrows, works directly from a hardware keyboard.

### Clipboard

- ⌘C or ⌘X on the Mac (from the iPad keyboard) puts what the Mac copied on the iPad's clipboard: text, rich text (formatting and links) and images. Safari asks for a tap before taking an image; tap **Copy here** in the message that appears.
- ⌘V pastes the iPad's clipboard into the Mac, images and rich text included (a photo copied in Photos pastes into a Mac document).
- Any Mac clipboard change is sent to connected devices. Images (up to 8 MB) are only sent when you copy them, so a big screenshot never slows the stream.
- On Linux hosts, images work both ways but rich text arrives as plain text.
- Links made with `--no-clipboard` get none of this.

### Sharp text

Turn on **Displays › Sharp text** when reading code or documents. The display streams at its native size with fewer but finer frames, in HEVC 4:2:2 colour where the Mac's encoder and the browser allow it, so coloured text keeps its edges. Once the screen stops changing, Glass sends a lossless full-colour (4:4:4) picture of it, so still text is pixel-exact; the next change resumes video. It's per viewer and remembered per computer. Elsewhere, `glass config hevc on` uses HEVC for everyone when every connected viewer can decode it.

## Console

`https://<computer>.<tailnet>.ts.net/admin/` (or **Computer › Console** in the viewer) is the owner's console, open to your own tailnet devices and the access token. It shows who's connected (and ends sessions), connection requests to allow or deny, one-time links with QR codes and limits, devices let in by links, connection history, settings, access rules, and an address book: your machines in groups you name, each opening in the viewer.

## Teams: sign-in, access rules, posture

**Single sign-on.** Register Glass with any OpenID Connect provider (Google, Microsoft Entra ID, Okta, Auth0, Keycloak…) with redirect URL `https://<computer>/sso/callback`, then:

```sh
glass config oidc-issuer https://accounts.google.com
glass config oidc-client-id <id>
glass config oidc-client-secret <secret>
```

The viewer then offers **Sign in with your organization** instead of asking for a token.

**LDAP.** `glass config ldap-url ldaps://dir.example.com` (or `ldap://` with StartTLS), then either `ldap-user-dn uid={user},ou=people,dc=example,dc=com` to bind directly, or `ldap-base-dn` plus optional `ldap-bind-dn` / `ldap-bind-password` to search for the user. Groups come from `memberOf` (`ldap-group-attr` to change). The viewer asks for a directory username and password.

**Access rules.** Who may sign in, and what they may do, first match wins:

```sh
glass acl add group:eng --view-only --for 8h
glass acl add @example.com
glass acl add ana@example.com --ask
glass acl                 # list
glass acl remove 0        # by position
```

A rule matches an email, `@domain`, `group:name` or `*`, and takes the same limits as `glass link`. With no rules, nobody signs in with a directory account. Rules live in `~/.config/glass/acl.json`.

**People on your tailnet.** The rules apply to Tailscale too: someone else on the computer's tailnet (a member you invited, or a device you shared it with) whose Tailscale login a rule names connects without the token, as a guest with that rule's limits. `glass config tailnet-members on` lets everyone on the tailnet in that way (off by default; on a company tailnet that's everyone in it). Devices tagged in Tailscale belong to no one and never count.

**SCIM.** `glass config scim-token new` prints a token for SCIM 2.0 provisioning at `https://<computer>/scim/v2` (Okta, Entra ID). Users and groups stay in sync; deactivated users can't sign in and lose their sessions at once.

**Device posture.** Checked on every connection:

- `posture-tailnet-tags tag:trusted`: tailnet devices must carry one of these Tailscale tags.
- `posture-os macos,ios`: only these operating systems.
- `posture-min-browser 120`: oldest browser major version let in.
- `posture-require-passkey-uv on`: passkey links need Face ID, Touch ID or a PIN, not just a tap.

## Branding and self-hosting

`glass config name "Studio"` and `glass config accent "#ff8800"` change the name and colour shown in the viewer. To run remote access without glassdesk.dev, deploy the introducer and TURN relay from [site/selfhost](https://github.com/Ntarasiuk/glass/tree/main/site/selfhost) and point Glass at it with `glass config relay https://glass.example.com`.

## CLI reference

```
glass               Run the host in the foreground (the background service runs this)
glass app           Open the Glass app (macOS); --install just adds Glass.app to ~/Applications
glass setup         Guided setup: Tailscale, background service, permissions, connect
glass setup --yes [--json]   Unattended setup for scripts and agents; reports what's left
glass status [--json]        Is the service running? Capturing? Permissions? Ready from anywhere? Connect URL
glass link [--for 24h]   One-time magic link (and QR code) for a device outside your tailnet
glass link --anywhere    The same, working from any network (WebRTC via glassdesk.dev)
glass link --view-only --no-files --no-clipboard --ask --passkey   Links with limits
glass link --list        Devices let in with links, with ids
glass link --revoke [id] Cut off one device, or every device let in with a magic link
glass viewers [--json]   Who's connected now
glass kick [id]          End one session, or every one
glass requests / allow [id] / deny [id]   Connection requests waiting for an answer
glass chat "…"           Message everyone viewing
glass notifications      Devices that get notifications (test, off <id>)
glass mic [install|uninstall]  Viewers' microphones here: status, set up, or remove the driver
glass support            Get help: a code for someone at glassdesk.dev/help (nothing stays afterwards)
glass privacy on|off     Blank this screen and block local input while viewed
glass log [-n 20] [--json]   Connection history
glass wake <name|mac>    Wake a machine with Wake-on-LAN
glass acl [add|remove]   Access rules for SSO and LDAP accounts
glass config [key [value]]   Settings (no arguments: list them all with descriptions)
glass remote on|off      Remote access from outside your tailnet (status without an argument)
glass account add        One-time link (and QR code) that puts this computer on your glassdesk.dev/computers account
glass account remove     Take this computer off every account
glass fleet [on|off|status|report]  Tell your account this computer's state; off deletes what was sent
glass fleet manage [off|status]  Let your account run actions here, each signed by your passkey (off forgets them)
glass token         Show the access token for devices outside your tailnet
glass token --reset Replace it with a new random token
glass token --set   Choose your own token (prompted; or piped: echo … | glass token --set)
glass mcp [--machine NAME]   MCP server letting an AI agent use this computer (or NAME on your tailnet)
glass agents [list|revoke ID|install CLIENT|log|verify|on|off|public on|off]  Agents from anywhere
glass feedback "…"  Send feedback to the developers
glass restart       Restart the background service (viewers reconnect by themselves)
glass uninstall     Stop the background service and remove it from login items
glass --version
```

Host options: `--bind` (repeatable, or comma-separated) sets the address for the web ports and WebTransport together. The default is `0.0.0.0` (every IPv4 address; WebTransport also accepts IPv6, as before). `tailscale` listens on this machine's Tailscale addresses and fails if Tailscale isn't running. For example `--bind tailscale --bind 127.0.0.1`, or `--bind 127.0.0.1,100.64.0.2`. The same list is saved with `glass config bind` and applies the next time Glass starts; a `--bind` flag overrides it. On macOS, port 443 needs the default bind (or `0.0.0.0`): an unprivileged process can only bind a port below 1024 on the wildcard address, so a specific address falls back to the web port. `--http-port` (default 8080; if it's taken, the next free port up to 8099, on every chosen address), `--https-port` (default 443: also serve there when free, so URLs need no port; 0 turns it off), `--wt-port` (UDP, default 4433; likewise moves to the next free port, which browsers learn automatically), `--fps` (default 60), `--mbps` (default 30), `--scale`, `--cert`/`--key` (custom TLS), `--token-file`, `--web-dir`, `--log-dir`.

Files:

- `~/.glass`: installed binary and web client
- `~/.config/glass/token`: access token (created on first run)
- `~/.config/glass/access.json`: magic links and guest passes (fingerprints only)
- `~/.config/glass/settings.json`: settings from `glass config`
- `~/.config/glass/acl.json`: access rules
- `~/.config/glass/connections.jsonl`: connection history
- `~/.config/glass/oidc-secret`, `ldap-bind-password`, `scim-token`: secrets, owner-only
- `~/Movies/Glass/` (`~/Videos/Glass/` on Linux): recordings
- `~/.config/glass/certs/`: Tailscale HTTPS certificate
- `~/.config/glass/status.json`: live service status
- `~/Library/Logs/Glass/`: host, client, and agent logs
- `~/Library/LaunchAgents/dev.glassdesk.host.plist`: background service

## How it works

- **Capture:** ScreenCaptureKit, zero-copy IOSurface frames, only when the screen changes. A static screen costs nothing.
- **Encode:** VideoToolbox hardware H.264 (HEVC when enabled and every viewer can decode it; 4:2:2 for Sharp text) in low-latency rate-control mode, with no B-frames. Keyframes are sent on demand (new viewer or detected loss) and at least every 900 frames.
- **Transport:** WebTransport (HTTP/3 over QUIC). Video and audio travel as unreliable datagrams, so a lost packet never blocks newer ones. Input, control, clipboard, and files use reliable streams.
- **Decode and render:** WebCodecs hardware decoding in a Web Worker, drawn to an OffscreenCanvas. The page's main thread only handles input.
- **Audio:** 48 kHz PCM in 5 ms packets, played through an AudioWorklet with a ~40 ms jitter buffer.
- **Integrity and recovery:** each frame carries a checksum; corrupted or incomplete frames are never decoded. The client requests a keyframe and recovers within a fraction of a second. If the browser's hardware decoder misbehaves, Glass falls back to more conservative decoder settings and remembers the choice.
- **Lossless refresh:** in Sharp text, once the screen is still, a PNG of the full frame (full 4:4:4 colour) is drawn over the video until the next change.
- **Clock sync:** NTP-style ping/pong over datagrams, used for the live latency display.

## Linux host (beta)

The same one-liner installs Glass on Linux x86_64 and ARM64 (aarch64). The Linux host uses:

- **Screen capture:** xdg-desktop-portal ScreenCast over PipeWire. On Hyprland (Omarchy), the share picker asks once which screen to share; Glass saves a restore token so it doesn't ask again.
- **Encoding:** software H.264 (OpenH264, screen-content mode). Screens larger than 2560×1600 are halved before encoding, to keep the frame rate up. Hardware VA-API encoding is planned.
- **Input:** a virtual keyboard and absolute pointer through `/dev/uinput`. `glass setup` installs a one-line udev rule (`/etc/udev/rules.d/60-glass-uinput.rules`) giving the logged-in user access; this needs `sudo` once.
- **Address:** Linux only lets root listen on port 443, so `glass setup` runs `tailscale serve` to forward `https://<machine>.<tailnet>.ts.net/` to Glass on localhost. If that isn't possible, use the `:8080` address. `--bind` / `glass config bind` chooses which addresses the ports use; include `127.0.0.1` when `tailscale serve` is what publishes port 443, because that forward targets localhost.
- **Clipboard:** `wl-copy` / `wl-paste` (wl-clipboard).
- **Audio:** the default output's monitor, through PipeWire. The viewer's microphone appears as a "Glass microphone" input while it's on.
- **Controllers:** game controllers on the viewing device appear as a virtual Xbox 360 controller through `/dev/uinput`.
- **Sharp text:** the lossless full-colour refresh works on Linux; HEVC 4:2:2 is macOS only.
- **Background service:** a systemd user unit (`~/.config/systemd/user/glass.service`) tied to the graphical session. Logs: `journalctl --user -u glass` and `~/.local/state/glass/`.
- **Tailscale:** non-root users need to be Tailscale operators to fetch HTTPS certificates; setup offers to run `sudo tailscale set --operator=$USER`.

Dependencies: PipeWire, wl-clipboard and your desktop's screen-sharing portal (`xdg-desktop-portal-hyprland`, `-gnome`, `-kde` or `-wlr`). Setup detects pacman, apt, dnf or zypper and offers to install what's missing, e.g. on Arch / Omarchy: `sudo pacman -S --needed pipewire xdg-desktop-portal-hyprland wl-clipboard`; on Ubuntu: `sudo apt-get install pipewire xdg-desktop-portal-gnome wl-clipboard`. Tailscale: `curl -fsSL https://tailscale.com/install.sh | sh`, then `sudo tailscale up`.

Keys arrive as typed on Linux hosts: ⌘ on an Apple keyboard is Super. Paste from the viewer's clipboard sends Ctrl+V.

While the session is locked (logind's lock hint, or a locker such as hyprlock or swaylock running), keys aren't shown to viewers or logged. **Stay awake with the lid closed** works on Linux laptops too; see [A locked screen, or a closed lid](#a-locked-screen-or-a-closed-lid).

## Windows host (beta)

On Windows 10 or 11 (x64 or Arm), run in PowerShell:

```powershell
irm glassdesk.dev/install.ps1 | iex
```

It installs to `%LOCALAPPDATA%\Programs\Glass` for your user (no administrator needed), starts Glass at sign-in, and runs `glass setup`. For managed installs there's a per-machine MSI (`msiexec /i glass-….msi /qn`) and Group Policy settings; see the release notes. The Windows host uses:

- **Screen capture:** DXGI Desktop Duplication, every display, with the pointer drawn in.
- **Encoding:** H.264 on the GPU through Media Foundation where there is one, otherwise OpenH264.
- **Input:** `SendInput`, keys by scan code. Game controllers need the free [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases) driver.
- **Clipboard:** text, rich text (HTML) and images.
- **Audio:** system audio through WASAPI loopback; the viewer's microphone through [VB-CABLE](https://vb-audio.com/Cable/) if it's installed.
- **Terminal:** PowerShell in the viewer, through ConPTY.
- **Background:** `glassw.exe` starts Glass at sign-in (the Run key), with an icon in the notification area showing who's viewing. Logs: `%LOCALAPPDATA%\Glass\Logs`.
- **Firewall:** `glass setup` adds an inbound rule named Glass on private networks. With `bind` set to `tailscale` that rule allows only 100.64.0.0/10 and fd7a:115c:a1e0::/48. A loopback-only bind removes it. The default stays any remote address on private networks. Changing `bind` updates the rule when Glass is already running as administrator, and otherwise says to run `glass setup`. The MSI restores one machine-wide scope (`HKLM\SOFTWARE\Glass` FirewallScope) on a silent install or upgrade. With no saved scope, which is every upgrade from an older MSI, it recreates the private-profile, any-address rule. The last `glass setup` on the computer writes that scope, and a later upgrade restores it; a missing or unusable value is not written back as `any`. Each user's bind stays in their own settings. A full MSI uninstall removes the saved scope. `glass uninstall --everything` removes it only when Glass is not installed for everyone. The copy at `Program Files\Glass\Service` gets its rule only from `glass service install`, so run that again after changing bind.
- **Lock screen, UAC, Ctrl+Alt+Del and signing in after a restart (optional):** `glass service install` (one UAC prompt) adds the Glass Secure Desktop service, which keeps a helper running as SYSTEM in the console session so viewers can see and use the lock screen, UAC prompts and elevated windows, and send Ctrl+Alt+Del. Only the signed-in console user's own Glass can talk to it. After a restart, while nobody is signed in, the service also serves the sign-in screen to the same viewers (from a copy of your token and settings that only SYSTEM and administrators can read): screen, keyboard and mouse only, no files, clipboard, terminal or agents. Once you sign in, your own Glass takes over and the page reconnects. `glass service uninstall` removes it.

To remove Glass, uninstall it in **Settings › Apps › Installed apps**: that takes away the programs, the start-at-sign-in entry, the Start menu shortcut, the firewall rule and the secure-desktop service (one UAC prompt), and keeps your settings in `%APPDATA%\Glass`. `glass uninstall` only stops Glass starting at sign-in.

## Viewing from Linux or Windows (Omarchy included)

Open the Glass URL in Chromium, Chrome, or Firefox with Tailscale running. When the host is a Mac, **PC keyboard** mode (on by default for Linux and Windows browsers; toggle it in the ⌘ menu) translates PC conventions:

- Ctrl → ⌘ (so Ctrl+C, Ctrl+V, Ctrl+Tab, and Ctrl+Q do what you expect on the Mac)
- Super → Ctrl
- Alt → ⌥

Hyprland keeps its own Super shortcuts. In fullscreen (Ctrl+Shift+F), Chromium's Keyboard Lock passes everything else through. If the browser has no hardware H.264 decoder, Glass uses software decoding automatically.

## Security and privacy

- All traffic is encrypted (TLS 1.3 via QUIC/HTTPS).
- With Tailscale, traffic stays on your private tailnet and goes directly between devices when possible.
- Access control: a device is let in if Tailscale reports it belongs to the same user as the Mac, if it presents the access token, or (as a guest) if its Tailscale user is named by an access rule or `tailnet-members` is on. Glass sends nothing to a session (video, audio, clipboard, input) until it is authenticated. Links, single sign-on, LDAP, access rules, posture checks and passkeys can narrow that further; see above.
- Glass runs as your user (a LaunchAgent on macOS, a systemd user service on Linux, a sign-in entry on Windows), never as root. On Windows you can opt into the Glass Secure Desktop service, whose SYSTEM helper only serves your own Glass (see [Windows host](#windows-host-beta)). The one exception you can opt into: **Stay awake with the lid closed** on a Mac installs a sudoers rule that lets it turn sleep off and on (`pmset -a disablesleep 0|1`), nothing else.
- Keys pressed into a password field, or anywhere while a Linux session is locked, aren't shown to viewers or logged.
- The file browser is for the computer's owner only (not links or guests), sees only the home folder (no `..`, no symlinks out), and moves to the Trash rather than deleting.
- Fleet (opt-in) sends a computer's details to your glassdesk.dev account and lets that account ask for a fixed set of actions, each signed by your passkey and checked by the computer itself. See [Glass Fleet](#glass-fleet-new) for what's sent and what can and can't happen.
- Notifications are the one thing that leaves your network: the computer sends them through your browser's push service (Apple, Google, Mozilla or Microsoft), encrypted end to end (RFC 8291), so the push service can't read them. Only devices you turn them on for get them; guests only ever get messages.
- The glassdesk.dev website uses Google Analytics on its pages (not on the remote viewer at glassdesk.dev/app), counts downloads using a salted hash of the IP address (no raw IPs stored) and stores feedback you choose to send, which is emailed to the maintainer. Downloads are also counted in Google Analytics under that same hash, with the country. The installer says which version it replaces, if any, so updates are counted apart from new installs. The Glass app itself sends nothing to glassdesk.dev except feedback you submit, its check for a new version, and, when you run `glass account add`, the computer's name and public key (plus, only if you run `glass fleet on`, the details listed under *Glass Fleet*). The /computers page loads no analytics.

## Code signing policy

Glass's programs are signed in GitHub Actions ([release.yml](https://github.com/Ntarasiuk/glass/blob/main/.github/workflows/release.yml)), built from this repository's source and nothing else: macOS builds with Glass's own key, Windows builds (`glass.exe`, `glassw.exe` and the MSI) with an Authenticode certificate. Only Glass's own code is signed, never third-party programs.

Windows signing is being set up through SignPath Foundation's free program for open-source projects. Until it's in place, the Windows downloads are unsigned: SmartScreen may warn before the MSI runs (**More info › Run anyway**), and a PC with Smart App Control turned on may block Glass. Each release's notes say whether its Windows builds are signed.

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

- **Committers and reviewers:** [Nathan Tarasiuk](https://github.com/Ntarasiuk). A pull request from anyone else is reviewed by a committer before it's merged.
- **Approvers:** [Nathan Tarasiuk](https://github.com/Ntarasiuk) approves every signing request by hand.
- **Privacy:** what Glass sends and where is under [Security and privacy](https://glassdesk.dev/docs#security-and-privacy).

## Requirements

- Host: macOS 13 Ventura or newer (Apple Silicon or Intel), or Linux (x86_64 or ARM64) with a Wayland compositor that supports the ScreenCast portal, plus PipeWire.
- Viewer: a browser with WebTransport and WebCodecs, such as Safari 26.4+ on iPadOS/iOS/macOS, or current Chrome, Edge, or Firefox.
- Remote access: Tailscale on both devices with HTTPS Certificates enabled for the tailnet (free plan works).

## Troubleshooting

- **"Glass isn't running":** run `glass status`, then check `~/Library/Logs/Glass/agent.log` and `host.log`.
- **Restarting Glass:** **Settings › Service › Restart** in the Glass app, `glass restart` on the computer, or **Computer › Restart Glass** in the viewer. After turning on HTTPS Certificates for your tailnet (or moving the computer to another tailnet), run `glass setup` instead: it fetches the new certificate and restarts the service.
- **Asked for a token on someone else's computer:** Glass lets in only devices signed into Tailscale as the computer's owner. Ask them to run `glass link` and send you the link (or give you the token from `glass token`).
- **Black screen after connecting:** Glass sends the current frame on connect; if it persists, check Screen Recording permission (`glass status`) and re-run `glass setup`.
- **Mouse and keyboard don't work:** Accessibility permission is missing. Re-run `glass setup` or enable Glass under System Settings → Privacy & Security → Accessibility.
- **After updating, permissions are asked again:** releases are signed with the same identity, so macOS keeps Screen Recording and Accessibility across updates. Glass you built yourself, or a copy you run from a terminal, is a different app to macOS and needs its own grant. Re-run `glass setup` to grant it.
- **iPad leaves fullscreen when swiping down:** a Safari fullscreen rule; use Add to Home Screen instead.
- **⌘Tab or ⌘Space doesn't reach the Mac:** iPadOS reserves them; use Ctrl+Tab / Ctrl+Space (Ctrl as ⌘) or the ⌘ menu.
- **"connecting…" forever:** both devices must be on Tailscale, and the URL must use the Mac's `.ts.net` name (not an IP), because the certificate is issued for that name.
- **The mic needs setting up:** run `glass mic` to check and `glass mic install` to set it up (Glass Microphone on a Mac, `pactl` on Linux). Another loopback driver works too on a Mac: set `GLASS_MIC_DEVICE` to part of its name in the service's environment.
- **The Mac's input is still Glass Microphone (or BlackHole):** Glass puts your previous input back when the mic turns off, and at its next start if it stopped while a mic was on. Or pick it in System Settings › Sound › Input.
- **Displays missing:** sleeping displays aren't capturable; Glass keeps displays awake while running and wakes them on start.
- **The lock screen says the password is incorrect when it isn't:** update Glass (`glass update`). Older versions' typing didn't count as someone at the computer, so while the display slept macOS rejected the password without checking it.
- **A MacBook goes to sleep when its lid closes:** turn on **Stay awake with the lid closed** (`glass config lid-awake on`) and keep it plugged in. `pmset -g | grep SleepDisabled` shows `1` while it's in effect.

## FAQ

**Is Glass free?** Yes, and open source under the MIT license: https://github.com/Ntarasiuk/glass

**Do I need to install anything on the iPad?** No, just a browser. Tailscale is optional: a one-time link from `glass link --anywhere` works without it.

**Can I use it without Tailscale?** Yes. `glass link --anywhere` makes a one-time link that connects any browser directly over WebRTC, end-to-end encrypted, with no open ports. On a tailnet, Glass uses WebTransport with a certificate Tailscale provides; advanced users can pass their own with `--cert`/`--key`.

**How is it different from Chrome Remote Desktop?** Chrome Remote Desktop uses a video-call pipeline tuned for smoothness. Glass is tuned for latency: on a tailnet it skips WebRTC entirely (UDP-style WebTransport datagrams, hardware decode straight to canvas, no jitter buffer), and from anywhere it uses WebRTC with the smallest jitter buffer the browser allows. Glass also streams multiple displays at once, transfers files, and recovers from iPad tab suspension automatically.

**How is it different from Parsec, Moonlight, or Jump Desktop?** Those need a native app on the viewing device. Glass runs in a browser, aims for similar latency, and is built for Mac and Linux hosts.

**Does it work on Linux?** Yes, as a viewer in any modern browser, and as a host on Wayland (beta).

**Does it work on Windows?** As a viewer, yes, in any modern browser. As a host, in beta: see [Windows host](#windows-host-beta).

**Does it stream audio?** Yes, the host's system audio, toggled per device.

**Can I control a locked Mac?** Yes; type your password at the lock screen. With **Stay awake with the lid closed** on, that works on a closed MacBook too, while it's plugged in. FileVault still requires one local login after a reboot.

## Feedback

- From the Mac: `glass feedback "what happened or what you'd like"`
- On the website: https://glassdesk.dev/features#feedback
- API: `POST https://glassdesk.dev/api/feedback` with JSON `{ "message": "…", "email": "optional" }`
- GitHub issues: https://github.com/Ntarasiuk/glass/issues
