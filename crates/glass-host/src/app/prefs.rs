//! Settings: every `glass config` setting, the access token, the background service, and
//! updates and feedback.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::*;

use super::ui::{self, I};
use super::{GlassApp, Store, act, actions};
use crate::settings;

/// Settings typed in, by key. Secrets are masked and never shown back.
const TEXT: &[&str] = &[
    "recordings-dir",
    "relay",
    "forward-ports",
    "bind",
    "name",
    "accent",
    "oidc-issuer",
    "oidc-client-id",
    "oidc-client-secret",
    "oidc-allow",
    "ldap-url",
    "ldap-user-dn",
    "ldap-base-dn",
    "ldap-user-filter",
    "ldap-bind-dn",
    "ldap-bind-password",
    "ldap-group-attr",
    "posture-tailnet-tags",
    "posture-os",
    "posture-min-browser",
];

pub fn secret(key: &str) -> bool {
    matches!(key, "oidc-client-secret" | "ldap-bind-password")
}

/// `glass config`'s help line, as a placeholder.
fn help(key: &str) -> &'static str {
    settings::KEYS.iter().find(|(k, _)| *k == key).map_or("", |(_, h)| h)
}

impl GlassApp {
    pub(super) fn make_fields(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = settings::load();
        for &key in TEXT {
            let value = if secret(key) { String::new() } else { current.get(key).unwrap_or_default() };
            let placeholder = if secret(key) { "Not set".to_string() } else { format!("e.g. {}", example(key)) };
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).masked(secret(key)).default_value(value));
            self._subs.push(cx.subscribe_in(&state, window, move |this, state, ev: &InputEvent, window, cx| {
                if matches!(ev, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit(key, state.clone(), window, cx);
                }
            }));
            self.fields.insert(key, state);
        }
    }

    /// Saves a typed setting when it changed.
    fn commit(&mut self, key: &'static str, state: Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        let value = state.read(cx).value().trim().to_string();
        if secret(key) {
            if value.is_empty() {
                return;
            }
            state.update(cx, |s, cx| s.set_value("", window, cx));
        } else if settings::load().get(key).unwrap_or_default() == value {
            return;
        }
        cx.spawn_in(window, async move |_, cx| {
            let r = cx.background_executor().spawn({
                let value = value.clone();
                async move { actions::set_setting(key, &value) }
            }).await;
            let _ = cx.update(|window, cx| {
                match r {
                    Ok(note) => {
                        // Show it as stored (normalized, e.g. lowercased).
                        if !secret(key) {
                            let stored = settings::load().get(key).unwrap_or_default();
                            state.update(cx, |s, cx| s.set_value(stored, window, cx));
                        }
                        let message = if note.is_empty() { "Saved.".into() } else { note };
                        super::toast(window, cx, Ok(message));
                    }
                    Err(e) => super::toast(window, cx, Err(e)),
                }
                Store::refresh(cx);
            });
        })
        .detach();
    }

    /// A text setting's row.
    pub(super) fn field(&self, key: &'static str, title: &str, detail: &str, cx: &App) -> AnyElement {
        let locked = self.store.read(cx).snap.policy.locked(key);
        let input = Input::new(&self.fields[key]).small().w(px(250.)).disabled(locked).when(secret(key), |i| i.mask_toggle());
        ui::row(title.to_string(), ui::managed(detail, locked), input, cx)
    }

    /// An on/off setting's row.
    pub(super) fn toggle(&self, key: &'static str, title: &str, detail: &str, on: bool, cx: &App) -> AnyElement {
        let locked = self.store.read(cx).snap.policy.locked(key);
        ui::row(
            title.to_string(),
            ui::managed(detail, locked),
            Switch::new(key).checked(on).disabled(locked).on_click(move |on: &bool, window, cx| {
                let v = if *on { "on" } else { "off" };
                act(window, cx, move || actions::set_setting(key, v))
            }),
            cx,
        )
    }

    pub(super) fn settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let snap = store.snap.clone();
        let update = store.update_available().map(str::to_string);
        let theme = cx.theme().clone();
        let s = &snap.settings;

        let mut connections = vec![
            self.toggle("ask-guests", "Ask before guests connect", "Someone with a link waits until you allow them.", s.ask_guests, cx),
            self.toggle("ask-everyone", "Ask before anyone connects", "Your own devices too.", s.ask_everyone, cx),
            self.toggle("tailnet-members", "Let everyone on this tailnet in", "Without the token, as guests. Otherwise only your own devices, and the people your access rules name.", s.tailnet_members, cx),
            self.toggle("privacy", "Privacy screen while viewed", "Blank this screen and block its keyboard and mouse.", s.privacy, cx),
            ui::row(
                "Reach from anywhere",
                ui::managed("Links can work outside your tailnet, over WebRTC through glassdesk.dev.", snap.policy.locked("remote")),
                Switch::new("remote-s").checked(snap.remote).disabled(snap.policy.locked("remote")).on_click(|on: &bool, window, cx| {
                    let on = *on;
                    act(window, cx, move || actions::set_remote(on))
                }),
                cx,
            ),
            ui::row(
                "Microphone from your devices",
                match &snap.mic {
                    Some(name) if cfg!(windows) => format!("Ready: a viewer's mic plays into {name}; pick it as the microphone in your apps."),
                    Some(name) => format!("Ready: a viewer's mic plays into {name}, the Mac's default input while it's on."),
                    None if cfg!(windows) => "Needs VB-CABLE, a free virtual audio cable. Install… opens its installer (then restart the computer).".into(),
                    None => "Needs Glass Microphone, a small audio driver. Installing asks for your Mac's password once.".into(),
                },
                match &snap.mic {
                    Some(_) => ui::status("Ready", theme.success).into_any_element(),
                    None => Button::new("install-mic")
                        .small()
                        .icon(I::Download)
                        .label("Install…")
                        .on_click(|_, window, cx| act(window, cx, || crate::mic::install(crate::mic::Ask::Dialog)))
                        .into_any_element(),
                },
                cx,
            ),
            self.field("relay", "Your own relay", "A self-hosted server for reaching from anywhere.", cx),
            self.field("forward-ports", "Forwarded ports", "Local web apps your devices may open through Glass.", cx),
            self.field(
                "bind",
                "Listen addresses",
                if cfg!(windows) {
                    "Where Glass accepts connections. Blank (shown as 0.0.0.0) is every IPv4 address. tailscale is this computer's Tailscale addresses, or list them: 127.0.0.1, 100.64.0.2. Restart to apply. glass setup updates the Windows Firewall rule to match."
                } else {
                    "Where Glass accepts connections. Blank (shown as 0.0.0.0) is every IPv4 address. tailscale is this Mac's Tailscale addresses, or list them: 127.0.0.1, 100.64.0.2. On macOS, port 443 needs the default bind or 0.0.0.0. Restart to apply."
                },
                cx,
            ),
        ];
        let lid_detail = if cfg!(windows) {
            "While plugged in, closing the lid does nothing, so you can keep using it from a viewer."
        } else {
            "While on power, so you can still unlock it from a viewer. Asks for your password once."
        };
        connections.insert(3, self.toggle("lid-awake", "Stay awake with the lid closed", lid_detail, s.lid_awake, cx));

        // The same switches as on AI agents, here too so they're easy to find and turn off.
        let agent = &snap.agent;
        let agents = vec![
            ui::row(
                "Agents from other devices",
                ui::managed("AI agents elsewhere (Vercel, claude.ai, a server) may ask to use this computer; you approve each once. Off refuses them all and keeps their grants.", snap.policy.locked("agents")),
                Switch::new("settings-agents-on").checked(agent.enabled).disabled(snap.policy.locked("agents")).on_click(|on: &bool, window, cx| {
                    let on = *on;
                    act(window, cx, move || actions::agents_enabled(on))
                }),
                cx,
            ),
            ui::row(
                "Publish for agents anywhere",
                "Just the agent sign-in and MCP endpoints, with Tailscale Funnel.",
                Switch::new("settings-agents-public").checked(agent.public).disabled(!agent.enabled).on_click(|on: &bool, window, cx| {
                    let on = *on;
                    act(window, cx, move || actions::agents_public(on))
                }),
                cx,
            ),
        ];

        let rec_dir = settings::recordings_dir(s);
        let media = vec![
            self.toggle("hevc", "Prefer HEVC", "Sharper at the same bandwidth, when every viewer supports it.", s.hevc, cx),
            self.toggle("record", "Record every session", "Saved as video files on this computer.", s.record, cx),
            ui::row(
                "Recordings",
                rec_dir.display().to_string(),
                h_flex()
                    .gap_2()
                    .child(Button::new("choose-rec").small().label("Choose…").on_click(cx.listener(|_, _, window, cx| {
                        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
                        cx.spawn_in(window, async move |_, cx| {
                            if let Ok(Ok(Some(paths))) = paths.await
                                && let Some(dir) = paths.first()
                            {
                                let dir = dir.display().to_string();
                                let _ = cx.update(|window, cx| act(window, cx, move || actions::set_setting("recordings-dir", &dir)));
                            }
                        })
                        .detach();
                    })))
                    .child(Button::new("open-rec").small().ghost().icon(I::Folder).on_click(move |_, _, _| {
                        let _ = std::fs::create_dir_all(&rec_dir);
                        actions::open(&rec_dir.display().to_string())
                    })),
                cx,
            ),
        ];

        let brand = vec![
            self.field("name", "Name in the viewer", "Instead of this computer's name.", cx),
            self.field("accent", "Accent colour", "The viewer's highlight, like #5cb2ff.", cx),
        ];

        let token = snap.token.clone();
        let shown = if self.show_token { token.clone() } else { "•".repeat(token.len().clamp(8, 24)) };
        let token_rows = vec![
            ui::row(
                "Access token",
                "Devices outside your tailnet sign in with this.",
                h_flex()
                    .gap_1()
                    .child(ui::mono(shown).px_2().py_1().rounded(px(6.)).bg(theme.background).border_1().border_color(theme.border))
                    .child(
                        Button::new("show-token")
                            .small()
                            .ghost()
                            .icon(if self.show_token { I::EyeOff } else { I::Eye })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_token = !this.show_token;
                                cx.notify();
                            })),
                    )
                    .child(Button::new("copy-token").small().ghost().icon(I::Copy).on_click(move |_, window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(token.clone()));
                        super::toast(window, cx, Ok("Token copied.".into()));
                    })),
                cx,
            ),
            ui::row(
                "Choose a token",
                "At least 16 characters.",
                h_flex()
                    .gap_2()
                    .child(Input::new(&self.token_input).small().w(px(200.)).mask_toggle())
                    .child(Button::new("set-token").small().label("Set").on_click(cx.listener(|this, _, window, cx| {
                        let t = this.token_input.read(cx).value().to_string();
                        this.token_input.update(cx, |s, cx| s.set_value("", window, cx));
                        act(window, cx, move || actions::set_token(&t));
                    }))),
                cx,
            ),
            ui::row(
                "Make a new token",
                "Devices using the old one must sign in again.",
                Button::new("reset-token").small().danger().label("Reset…").on_click(|_, window, cx| {
                    confirm(window, cx, "Make a new access token?", "Devices that signed in with the old token must sign in again.", "Reset", actions::reset_token)
                }),
                cx,
            ),
        ];

        let service = vec![
            ui::row(
                "Background service",
                match (snap.running(), &snap.status) {
                    (true, Some(st)) => format!("Running · version {} · pid {}", st.version, st.pid),
                    _ if snap.installed => "Installed, but not running".into(),
                    _ => "Not installed".into(),
                },
                h_flex()
                    .gap_2()
                    .when(snap.installed, |d| {
                        d.child(Button::new("restart").small().icon(I::RotateCw).label("Restart").on_click(|_, window, cx| act(window, cx, actions::restart)))
                    })
                    .when(!snap.installed, |d| {
                        d.child(Button::new("install").small().primary().icon(I::Power).label("Turn on").on_click(|_, window, cx| {
                            act(window, cx, || actions::install(true))
                        }))
                    }),
                cx,
            ),
            ui::row(
                "Setup",
                "Walk through Tailscale, permissions and connecting again.",
                Button::new("setup-again").small().label("Run setup").on_click(cx.listener(|this, _, _, cx| {
                    this.setup = Some(super::setup::Setup::default());
                    cx.notify();
                })),
                cx,
            ),
            ui::row(
                "Logs",
                "What Glass did, for troubleshooting.",
                Button::new("logs").small().ghost().icon(I::FileText).label("Open").on_click(|_, _, _| {
                    actions::open(&crate::platform::log_dir().display().to_string())
                }),
                cx,
            ),
            ui::row(
                "Turn off Glass",
                if cfg!(windows) {
                    "Stops the service and removes it from startup apps. Your settings stay."
                } else {
                    "Stops the service and removes it from login items. Your settings stay."
                },
                Button::new("uninstall").small().danger().label("Turn off…").on_click(|_, window, cx| {
                    confirm(window, cx, "Turn off Glass?", "Nobody can connect until you turn it on again.", "Turn off", actions::uninstall)
                }),
                cx,
            ),
        ];

        let about = vec![
            ui::row(
                format!("Glass {}", crate::update::CURRENT),
                match (&update, crate::update::blocked()) {
                    (_, Some(why)) => why.into(),
                    (Some(v), None) => format!("Version {v} is available."),
                    (None, None) => "You're up to date.".into(),
                },
                h_flex()
                    .gap_2()
                    .when(update.is_some(), |d| {
                        d.child(Button::new("update").small().primary().icon(I::Download).label("Update").on_click(cx.listener(|this, _, window, cx| this.update_now(window, cx))))
                    })
                    .child(Button::new("docs").small().ghost().label("Docs").icon(I::ExternalLink).on_click(|_, _, _| actions::open("https://glassdesk.dev/docs"))),
                cx,
            ),
            v_flex()
                .px_4()
                .py_3()
                .gap_2()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Send feedback"))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.feedback).small()))
                        .child(Button::new("send-feedback").small().icon(I::Send).label("Send").on_click(cx.listener(|this, _, window, cx| {
                            let t = this.feedback.read(cx).value().to_string();
                            this.feedback.update(cx, |s, cx| s.set_value("", window, cx));
                            act(window, cx, move || actions::feedback(&t));
                        }))),
                )
                .into_any_element(),
        ];

        let mut notify: Vec<AnyElement> = snap
            .notify
            .iter()
            .map(|d| {
                let (test, off) = (d.id.clone(), d.id.clone());
                ui::icon_row(
                    I::Smartphone,
                    theme.primary,
                    d.device.clone(),
                    d.what.clone(),
                    h_flex()
                        .gap_2()
                        .child(Button::new(SharedString::from(format!("test-{}", d.id))).small().ghost().label("Test").on_click(move |_, window, cx| {
                            let id = test.clone();
                            act(window, cx, move || crate::push::test(Some(&id)).map(|n| if n > 0 { "Sent. Check that device.".into() } else { "That device didn't take it.".into() }))
                        }))
                        .child(Button::new(SharedString::from(format!("off-{}", d.id))).small().ghost().icon(I::Trash).on_click(move |_, window, cx| {
                            let id = off.clone();
                            act(window, cx, move || crate::push::remove(&id).map(|_| "Notifications off for that device.".into()))
                        })),
                    cx,
                )
            })
            .collect();
        if notify.is_empty() {
            notify.push(ui::note("None yet. On your phone or iPad, open Glass and tap the bell in its toolbar.", cx));
        }

        ui::page("Settings", "How Glass behaves, your access token, and the background service.", cx)
            .child(ui::section("Connections", connections, cx))
            .child(ui::section("AI agents", agents, cx))
            .child(ui::section("Notifications on your devices", notify, cx))
            .child(ui::section("Video and recording", media, cx))
            .child(ui::section("Branding", brand, cx))
            .child(ui::section("Access token", token_rows, cx))
            .child(ui::section("Service", service, cx))
            .child(ui::section("About", about, cx))
    }
}

/// An example value for an empty field.
fn example(key: &str) -> String {
    match key {
        "relay" => "https://glass.example.com".into(),
        "forward-ports" => "3000, 8888".into(),
        "bind" => "tailscale, 127.0.0.1".into(),
        "name" => "Studio Mac".into(),
        "accent" => "#5cb2ff".into(),
        "recordings-dir" => "~/Movies/Glass".into(),
        "oidc-issuer" => "https://accounts.google.com".into(),
        "oidc-client-id" => "1234-abc.apps.googleusercontent.com".into(),
        "oidc-allow" => "you@example.com, @example.com".into(),
        "ldap-url" => "ldaps://ldap.example.com".into(),
        "ldap-user-dn" => "uid={user},ou=people,dc=…".into(),
        "ldap-base-dn" => "dc=example,dc=com".into(),
        "ldap-user-filter" => "(uid={user})".into(),
        "ldap-group-attr" => "memberOf".into(),
        "posture-tailnet-tags" => "tag:trusted".into(),
        "posture-os" => "macos, ios".into(),
        "posture-min-browser" => "120".into(),
        _ => help(key).to_string(),
    }
}

/// A native confirmation, then the action.
pub fn confirm(
    window: &mut Window,
    cx: &mut App,
    title: &str,
    detail: &str,
    ok: &str,
    f: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
) {
    let answer = window.prompt(PromptLevel::Warning, title, Some(detail), &[ok, "Cancel"], cx);
    window
        .spawn(cx, async move |cx| {
            if answer.await == Ok(0) {
                let _ = cx.update(|window, cx| act(window, cx, f));
            }
        })
        .detach();
}
