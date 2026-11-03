---
name: glass
description: Install, set up, check or troubleshoot Glass (glassdesk.dev), a low-latency browser remote desktop, on any Mac or Linux machine in the user's Tailscale tailnet, over SSH; and hand out one-time Glass access links. Use when the user wants to control another computer from an iPad or browser, add a machine to Glass, set Glass up remotely, find out why Glass isn't working, or asks for a Glass link / access link / magic link (for themselves or someone else), or to revoke one.
---

# Glass: set up any machine on the tailnet

Glass streams a computer's screen to a browser and takes mouse, keyboard and touch input back. The host runs on macOS 13+ and on Linux x86_64 or ARM64 (Wayland). Every machine with Glass on the user's tailnet shows up in the client's **Machines** menu, so setting up one more machine is all it takes to control it from anywhere.

You can do almost everything remotely. Two steps need a person at the target's screen (macOS permission switches, or the Linux share picker); your job is to do the rest and tell the user exactly what to click.

## Ground rules

- Only set up machines the user owns: peers in `tailscale status` with the same `UserID` as `Self`. Never touch other people's devices on a shared tailnet.
- Confirm the machine with the user before installing (name and OS), then proceed without asking at each step.
- Use `sudo` only non-interactively (`sudo -n`). If something needs a password, report it; don't try to supply one.
- Don't change anything beyond what Glass setup does.

## 1. Find the machine

```sh
tailscale status --json
```

Candidates are `Self` and the `Peer` entries whose `UserID` equals `Self.UserID`, with `Online: true` and `OS` of `macOS` or `linux`. (iOS and Android devices can only view.) Use the `DNSName` without the trailing dot as the host, e.g. `studio.tail1234.ts.net`. If the user's request is ambiguous, list the candidates and ask.

## 2. Reach it

Try Tailscale SSH first, then ordinary SSH:

```sh
tailscale ssh <user>@<host> true   # Tailscale SSH (enable on the target: sudo tailscale set --ssh)
ssh -o BatchMode=yes <user>@<host> true   # OpenSSH (macOS: System Settings › General › Sharing › Remote Login)
```

The `<user>` is the login on the target, often the same as locally. If neither works, tell the user how to enable one of them on the target and stop. Below, `run` means whichever worked, e.g. `tailscale ssh me@studio.tail1234.ts.net '<command>'`.

## 3. Check what's there

```sh
run '~/.glass/bin/glass status --json || echo not-installed'
```

`{"running": true, "status": {"capturing": true, "accessibility": true, ...}}` means it's already working: skip to step 6.

## 4. Install or update

```sh
run 'curl -fsSL https://glassdesk.dev/install.sh | sh'
```

Without a terminal it installs and prints "Next: run glass setup" instead of starting the interactive setup. Re-running updates in place; on a machine that is already set up it prints "Updated Glass X → Y" and skips setup.

## 5. Set up without a human (as far as possible)

```sh
run '~/.glass/bin/glass setup --yes --json'
```

This gets the Tailscale HTTPS certificate, installs and starts the background service (a LaunchAgent on macOS, a systemd user service on Linux), installs Linux dependencies and the input rule when passwordless sudo allows, and opens the macOS permission panes on the target's screen. It prints a report:

```json
{
  "ok": false,
  "url": "https://studio.tail1234.ts.net/",
  "running": true, "capturing": false, "input": false,
  "needs": [
    { "step": "screen-recording", "action": "Allow \"glass\" in System Settings › …", "on_screen": true },
    { "step": "accessibility", "action": "Allow \"glass\" in System Settings › …", "on_screen": true }
  ]
}
```

Relay `needs` to the user in plain words, grouped:

- **`on_screen: true`**: someone at that machine clicks (or uses another remote tool briefly). On macOS the panes are already open there. On Linux the share picker appears when the service starts.
- **`on_screen: false`**: something you or the user can do remotely, e.g. enable HTTPS certificates in the Tailscale admin console, run a `sudo` command, install packages.

Common steps:

| `step` | Meaning | Who |
|---|---|---|
| `tailscale-https` | Turn on HTTPS Certificates at https://login.tailscale.com/admin/dns | user, once per tailnet |
| `tailscale-cert` | Linux: `sudo tailscale set --operator=$USER` on the target | you via SSH if sudo allows, else user |
| `dependencies` | Linux packages (exact command for the distro's package manager given) | you via SSH if sudo allows, else user |
| `input-access` | Linux uinput rule; `glass setup` on the target asks for sudo once | user |
| `screen-recording`, `accessibility` | macOS permission switches | person at the screen |
| `screen-share` | Linux share picker | person at the screen |
| `service` | Service didn't start; see the logs below | you |

If the service step fails over SSH on macOS, the user may not be logged in at the console (LaunchAgents need a GUI session). On Linux, `systemctl --user` needs a logged-in graphical session for the screen-share portal.

## 6. Wait for it, then verify

Poll every few seconds while the user clicks (give up after a few minutes and report):

```sh
run '~/.glass/bin/glass status --json'
```

Done when `running`, `status.capturing` and `status.accessibility` are all true. Then check it from this machine:

```sh
curl -fsS https://<host>:8080/config.json   # :8080 always works; the URL from status may have no port
```

`glass status --json` also has `remote_ready` (true when the computer is capturing, allowed to take input and connected to glassdesk.dev, so a link or account claim will work) and `remote_problems`, a list saying why not. Before giving the user a `--anywhere` link, check `remote_ready`.

Finish by telling the user the URL from `glass status --json` (usually `https://<host>/`, else `https://<host>:8080/`). It also appears in the **Machines** menu of any Glass tab, and devices on their tailnet connect without a token.

## Get someone a link

When the user asks for a link to a machine (for a device that isn't on their tailnet, or for someone else), make a one-time magic link on that machine. Run it locally if the machine is this one, otherwise over SSH as in step 2:

```sh
glass link --json            # or ~/.glass/bin/glass; add --for 1h / 7d to change how long access lasts (default 24h)
```

Add `--anywhere` when the device isn't on the user's tailnet (a phone on cellular, someone else's laptop): the link then works from any network, over WebRTC via glassdesk.dev, and turns remote access on (`glass remote off` turns it off). It prints `{"url": …, "link_expires": …, "grant_secs": …, "summary": …}`. Give the user the `url` and the `summary` ("Works on one device, within 15 minutes. Gives that device 24 hours of access."). The link is a credential: send it only to the user, in this conversation, never anywhere else, and don't store it. If the command says Glass isn't running, check `glass status` (section 6).

When the user wants one place to see and open all their computers from any device, run `glass account add --json` on each machine (it needs remote access on: `glass remote on`). It prints `{"url": …, "name": …, "expires": …}`; the user opens the `url` where they're signed in at glassdesk.dev/computers (with a passkey) within 15 minutes. The account only lists computers and whether they're online; opening one still takes that computer's own sign-in. `glass account remove --json` takes the machine off every account.

To cut off everyone who was let in with a link: `glass link --revoke --json`. The user's own token and tailnet devices are unaffected.

Limit a link with `--view-only`, `--no-files`, `--no-clipboard`, `--ask` (the person at the computer must allow each connection; answer with `glass requests`, `glass allow [id]`, `glass deny [id]`) or `--passkey` (the device registers a passkey and needs it every time). `glass link --list --json` shows devices let in by links with ids; `glass link --revoke <id> --json` cuts off one. Other host commands: `glass viewers --json`, `glass kick [id]`, `glass chat "…"`, `glass privacy on|off`, `glass log --json`, `glass wake <name|mac>`, and `glass config [key [value]]` for settings (ask-guests, ask-everyone, record, recordings-dir, privacy, hevc, lid-awake, forward-ports, bind, name, accent, relay). On a laptop the user means to keep closed, suggest `glass config lid-awake on`: it stays awake with the lid closed while plugged in, so it can still be unlocked from a viewer. On a Mac it asks for the admin password once (sudo in the terminal); on Linux it needs no password. `glass config bind` (or `--bind`, which overrides it for that run) chooses listen addresses, comma-separated; `tailscale` uses this machine's Tailscale addresses. The default is `0.0.0.0`. It applies the next time Glass starts. On macOS, port 443 needs the default bind (or `0.0.0.0`). On Windows the firewall rule follows the saved bind: `tailscale` allows only Tailscale, and `glass setup`, `glass service install`, `glass update`, or an MSI install applies a change. `--bind` does not change the rule, and `glass config bind` does not ask for administrator approval.

## Use the computer yourself (MCP)

Once Glass runs on a machine, you can see and use it through `glass mcp`, an MCP server with `screenshot`, `snapshot` (an app's window as text with refs, macOS), `press` and `set_value` (act on a ref without moving the pointer), `click`, `type_text`, `press_keys`, `scroll`, `drag`, `wait_for_change`, `request_help` and `confirm`. Suggest adding it when the user wants you to do something in an app with no API: `claude mcp add -s user glass -- glass mcp`. One server reaches every machine: `list_machines`, then pass `machine: "<name>"` to any tool. The user watches you from any Glass viewer and can pause or stop you. Always `confirm` before anything hard to undo, and use `request_help` for sign-ins, 2FA codes and CAPTCHAs instead of working around them.

## Troubleshooting

- Status and logs: `glass status`, plus `~/Library/Logs/Glass/` (macOS) or `~/.local/state/glass/` and `journalctl --user -u glass` (Linux). `client.log` holds errors reported by connected browsers.
- Permissions asked again after an update: macOS ties them to the exact binary; repeat step 5 and relay the on-screen steps.
- Black screen: `capturing` is false. Screen Recording (macOS) or the share approval (Linux) is missing.
- Mouse and keyboard ignored: `accessibility` is false.
- Can't connect: both devices need Tailscale, and the URL must use the `.ts.net` name the certificate was issued for.
- Full docs: https://glassdesk.dev/llms-full.txt
