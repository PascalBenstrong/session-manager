//! Settings view.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::section_card;
use super::{Actions, ViewCtx};
use crate::media::{MediaPolicy, REPLAY_FRAMES, REPLAY_HEIGHTS};
use crate::platform as pf;
use crate::protocol::Command;
use crate::tab_strip::{FIREFOX_EXTENSION_ID, USER_CHROME_PREF, sidebar_command_id, user_chrome_css};

pub fn show(ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
    let p = theme::palette(ui);
    let mut s = cx.state.settings.clone();
    let mut changed = false;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        section_card(
            ui,
            "Performance",
            "Features that work in every page or on every tab change. Turn off the ones you don't use to keep the browser light.",
            |ui| {
                let p = theme::palette(ui);
                let m = &mut s.media;
                changed |= setting(
                    ui,
                    &mut m.stream_controls,
                    "Stream controls",
                    "⏸ Stop stream on playing videos, in the sidebar and the popup, and the 🔊 of tabs playing media. \
                     Runs a small script in every page.",
                );
                ui.add_enabled_ui(m.stream_controls, |ui| {
                    changed |= setting(
                        ui,
                        &mut m.stop_replay,
                        "Replay of stopped streams",
                        "Keeps copies of the playing video's last frames (up to 10 a second) to loop them while it's stopped.",
                    );
                });
                let mut limit = m.enabled();
                if setting(ui, &mut limit, "Media limit", "Pauses videos in background tabs (choose how below, in Media playback).") {
                    m.policy = if limit { MediaPolicy::ActiveTabOnly } else { MediaPolicy::Off };
                    changed = true;
                }
                let g = &mut s.guards;
                let mut guards = g.popups || g.click_hijack || g.context_menu || g.anti_debugger || g.devtools_stealth;
                if setting(
                    ui,
                    &mut guards,
                    "Page guards",
                    "Scripts that run in every page before its own (pop-ups, play button ads, right-click, debugger traps, \
                     developer tools). Off turns them all off; choose them one by one in Guards.",
                ) {
                    if guards {
                        g.popups = true;
                        g.click_hijack = true;
                    } else {
                        g.popups = false;
                        g.click_hijack = false;
                        g.context_menu = false;
                        g.text_selection = false;
                        g.anti_debugger = false;
                        g.strip_debugger_scripts = false;
                        g.devtools_stealth = false;
                        g.silence_console = false;
                    }
                    changed = true;
                }
                if cx.caps.tab_groups {
                    changed |= setting(
                        ui,
                        &mut s.browser_ui.tab_groups,
                        "Workspaces as tab groups",
                        "Keeps every workspace's tabs in a tab group, checked after tab changes.",
                    );
                }
                let script = if s.media.needs_page_script() {
                    "The media page script runs in pages (for the stream controls or the media limit)."
                } else {
                    "No media script runs in pages."
                };
                ui.label(RichText::new(script).small().color(p.muted));
            },
        );

        section_card(ui, "Tab tracking", "How open tabs are recorded in sessions.", |ui| {
            changed |= setting(
                ui,
                &mut s.auto_add_tabs,
                "Add opened tabs to the active session",
                "New tabs, and tabs that navigate to a page, are saved automatically.",
            );
            changed |= setting(
                ui,
                &mut s.ignore_internal_urls,
                "Ignore browser-internal pages",
                "Skip about:, chrome://, extension pages and the new tab page.",
            );
            changed |= setting(
                ui,
                &mut s.keep_tabs_on_window_close,
                "Keep tabs when a window closes",
                "Tabs of a closed window, or of a browser that quits, stay in the session as unloaded. \
                 Closing a single tab always removes it.",
            );
        });

        section_card(
            ui,
            "Workspaces in the browser",
            "Show your workspaces in the browser's own tab bar, sidebar and window title.",
            |ui| {
                let caps = cx.caps;
                let b = &mut s.browser_ui;
                ui.add_enabled_ui(caps.tab_groups, |ui| {
                    changed |= setting(
                        ui,
                        &mut b.tab_groups,
                        "Show workspaces as tab groups",
                        "Each workspace's tabs sit in a native tab group with its icon, name and color. Dragging a tab into \
                     or out of a group moves it between workspaces.",
                    );
                });
                if !caps.tab_groups {
                    note(ui, "This browser doesn't let extensions make tab groups (Chrome, Edge and Firefox 139+ do).");
                }
                ui.add_enabled_ui(caps.tab_groups && b.tab_groups, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.vertical(|ui| {
                            changed |= setting(
                                ui,
                                &mut b.collapse_inactive,
                                "Collapse the other workspaces",
                                "Only the group of the workspace you're in stays open.",
                            );
                        });
                    });
                });

                if caps.hide_tabs {
                    let hover = ui
                        .scope(|ui| {
                            changed |= setting(
                                ui,
                                &mut b.hide_inactive,
                                "Hide the tabs of other workspaces",
                                "Only the current workspace's tabs (and tabs without a workspace) are in the tab bar. Switch \
                             workspaces from the sidebar or a workspace's settings.",
                            );
                        })
                        .response
                        .contains_pointer();
                    // Firefox asks for the "Hide and show browser tabs" permission (only on a click).
                    if hover && !caps.hide_tabs_allowed {
                        acts.arm("tabHide");
                    }
                    if b.hide_inactive && !caps.hide_tabs_allowed {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("⚠ Firefox needs your permission to hide tabs.").color(p.warn));
                            let r = ui.button("Allow hiding tabs");
                            if r.contains_pointer() {
                                acts.arm("tabHide");
                            }
                        });
                    }
                    changed |= setting(
                        ui,
                        &mut b.show_in_title,
                        "Show the current workspace in the window title",
                        "E.g. \"💼 Work — Page title\".",
                    );
                } else {
                    note(ui, "Hiding other workspaces' tabs needs Firefox. Here, collapse the other workspaces' groups instead.");
                }
                changed |= setting(
                    ui,
                    &mut b.new_tabs_join_active,
                    "New tabs join the current workspace",
                    "Tabs that no workspace rule places go to the workspace you're in.",
                );

                if caps.sidebar {
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        let r = ui.button("◨ Open the workspace sidebar");
                        if r.contains_pointer() {
                            acts.arm("sidebar");
                        }
                        let how = if caps.browser == "firefox" {
                            "Shortcut: Alt+Shift+W, or View → Sidebar → Session Manager."
                        } else {
                            "Shortcut: Alt+Shift+W, or pick Session Manager in the side panel menu."
                        };
                        ui.label(RichText::new(how).small().color(p.muted));
                    });
                }
                if caps.browser == "firefox" {
                    user_chrome_guide(ui, cx, acts);
                }
            },
        );

        section_card(ui, "Media playback", "Pause videos playing in background tabs, so only the tab you're looking at plays.", |ui| {
            let m = &mut s.media;
            for policy in MediaPolicy::ALL {
                ui.horizontal(|ui| {
                    if ui.radio(m.policy == policy, policy.label()).clicked() && m.policy != policy {
                        m.policy = policy;
                        changed = true;
                    }
                    if policy == MediaPolicy::LimitBackground {
                        ui.add_enabled_ui(m.policy == MediaPolicy::LimitBackground, |ui| {
                            ui.label(RichText::new("at most").color(p.muted));
                            egui::ComboBox::from_id_salt("max-background").selected_text(m.max_background.to_string()).width(50.0).show_ui(
                                ui,
                                |ui| {
                                    for n in [1, 2, 3, 4, 5, 10] {
                                        changed |= ui.selectable_value(&mut m.max_background, n, n.to_string()).changed();
                                    }
                                },
                            );
                            ui.label(RichText::new("background tab(s) play at once").color(p.muted));
                        });
                    }
                });
            }
            ui.add_space(6.0);
            ui.add_enabled_ui(m.enabled(), |ui| {
                changed |= setting(
                    ui,
                    &mut m.include_audio,
                    "Also limit audio",
                    "Music and podcasts count too. Otherwise only videos are paused.",
                );
                changed |= setting(
                    ui,
                    &mut m.resume_on_activate,
                    "Resume when you return",
                    "Media paused by the limit continues when you switch back to its tab.",
                );
                changed |=
                    setting(ui, &mut m.allow_pip, "Keep picture-in-picture playing", "A video shown picture-in-picture counts as visible.");
            });
            ui.label(
                RichText::new(
                    "Applies to pages loaded after the extension was installed or updated (reload older tabs). \
                         The active tab of every window may play; when more background tabs play than allowed, \
                         the ones you used least recently are paused.",
                )
                .small()
                .color(p.muted),
            );
        });

        section_card(
            ui,
            "Stopped streams",
            "⏸ Stop stream (on a playing video, or next to its tab in the sidebar) pauses it and holds back its data until you resume it.",
            |ui| {
                let m = &mut s.media;
                if !m.stream_controls {
                    ui.label(RichText::new("Stream controls are off (Performance, above).").small().color(p.muted));
                }
                ui.add_enabled_ui(m.stream_controls, |ui| {
                    changed |= setting(
                        ui,
                        &mut m.stop_replay,
                        "Replay the last frames while stopped",
                        "Loops a low-resolution copy of the video's last frames over it, kept in the page's memory (no data). \
                     It's only a picture: the site's player stays paused.",
                    );
                    ui.add_enabled_ui(m.stop_replay, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Resolution");
                            egui::ComboBox::from_id_salt("replay-height")
                                .selected_text(format!("{}p", m.replay_height))
                                .width(70.0)
                                .show_ui(ui, |ui| {
                                    for h in REPLAY_HEIGHTS {
                                        changed |= ui.selectable_value(&mut m.replay_height, h, format!("{h}p")).changed();
                                    }
                                });
                            ui.add_space(12.0);
                            ui.label("Frames");
                            changed |= ui.add(egui::DragValue::new(&mut m.replay_frames).range(REPLAY_FRAMES)).changed();
                            // ~0.4 MB per frame at 240p; videos keep their last frames while they play.
                            let mb = m.replay_frames as f64 * (m.replay_height as f64).powi(2) * 16.0 / 9.0 * 4.0 / 1e6;
                            ui.label(RichText::new(format!("≈ {mb:.0} MB of memory per playing video")).small().color(p.muted));
                        });
                    });
                });
                ui.label(
                    RichText::new(
                        "Firefox holds the stream's requests back, so the player just waits. Chrome can only block them: \
                         players see an error, and the page is reloaded if playback doesn't recover when you resume.",
                    )
                    .small()
                    .color(p.muted),
                );
            },
        );

        section_card(
            ui,
            "Recording & storage",
            "What is recorded as you browse, and how it's saved. Turning something off stops recording it; nothing already saved is deleted.",
            |ui| {
                let t = &mut s.tracking;
                changed |= setting(
                    ui,
                    &mut t.last_closed,
                    "Remember the last open tabs",
                    "When a window closes or the browser quits, the tabs that were open can be restored in one click (↺ Restore). \
                     Tabs you close yourself are left out.",
                );
                changed |= setting(
                    ui,
                    &mut t.last_used,
                    "Record when tabs were last used",
                    "Saved on every tab switch. Used by ⟲ Last tab, the \"Last used\" order and used: searches.",
                );
                changed |= setting(
                    ui,
                    &mut t.tab_times,
                    "Record when tabs open, close, load and unload",
                    "Used by the time orders and time searches (opened:, closed:, loaded:, unloaded:).",
                );
                changed |= setting(ui, &mut t.popup_log, "Log tabs opened by pages", "The list in Page guards that offers one-click rules.");
                changed |= setting(ui, &mut t.cleanup_log, "Log clean-ups", "The recent clean-ups list in Cookies & data.");
                changed |= setting(
                    ui,
                    &mut t.database,
                    "Save in the browser's database (IndexedDB)",
                    "A change writes only the tabs that changed. Off: everything is saved together in one place, as before                      (your tabs move there; nothing is lost).",
                );
            },
        );

        ai_card(ui, cx, acts, &mut s, &mut changed);

        let sessions = cx.state.sessions.len();
        let tabs: usize = cx.state.sessions.iter().map(|s| s.entries.len()).sum();
        section_card(ui, "About", "", |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Session Manager {}", crate::release_notes::VERSION)).strong());
                if ui.link("What's new").clicked() {
                    acts.navigate("whatsnew");
                }
            });
            ui.label(format!("{sessions} session(s), {tabs} saved tab(s)."));
            ui.label(
                RichText::new(format!(
                    "Browser: {}  ·  native tab groups: {}  ·  per-site cache clearing: {}",
                    if cx.caps.browser.is_empty() { "unknown" } else { &cx.caps.browser },
                    yes_no(cx.caps.tab_groups),
                    yes_no(cx.caps.per_host_cache),
                ))
                .color(p.muted),
            );
        });
    });

    if changed {
        acts.send(Command::UpdateSettings { settings: s });
    }
}

pub(crate) fn setting(ui: &mut egui::Ui, value: &mut bool, title: &str, description: &str) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = ui.checkbox(value, "").changed();
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(RichText::new(title).strong());
            ui.label(RichText::new(description).small().color(p.muted));
        });
    });
    ui.add_space(6.0);
    changed
}

fn note(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui);
    ui.label(RichText::new(text).small().color(p.muted));
    ui.add_space(4.0);
}

/// Steps to hide Firefox's horizontal tab bar with `userChrome.css`, so the sidebar is the
/// tab bar. Extensions can't change this themselves (nor open `about:config`), so it says
/// exactly what to change and copies what can be copied.
fn user_chrome_guide(ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
    let p = theme::palette(ui);
    let id = if cx.caps.extension_id.is_empty() { FIREFOX_EXTENSION_ID } else { &cx.caps.extension_id };
    let css = user_chrome_css(&sidebar_command_id(id));
    ui.separator();
    egui::CollapsingHeader::new(RichText::new("Hide Firefox's tab bar (userChrome.css)").strong()).id_salt("user-chrome").show(ui, |ui| {
        ui.label(
            RichText::new(
                "With the sidebar as your tab bar, Firefox's own tab bar can go. Firefox only allows that with a \
                     userChrome.css file in your profile, which extensions can't write for you. It takes four steps:",
            )
            .color(p.muted),
        );
        ui.add_space(6.0);

        step(ui, "1", "Allow userChrome.css");
        ui.label("Open about:config, accept the warning, search for this preference and set it to true:");
        ui.horizontal_wrapped(|ui| {
            code(ui, USER_CHROME_PREF);
            ui.label(RichText::new("→").color(p.muted));
            code(ui, "true");
            if ui.button("Copy name").clicked() {
                ui.ctx().copy_text(USER_CHROME_PREF.to_string());
                acts.notice("Preference name copied");
            }
            if ui.button("Open about:config").on_hover_text("If Firefox refuses, the address is copied instead").clicked() {
                acts.open_page("about:config");
            }
        });
        ui.add_space(6.0);

        step(ui, "2", "Open your profile folder");
        ui.horizontal_wrapped(|ui| {
            ui.label("Open about:support and, next to");
            code(ui, "Profile Folder");
            ui.label("click");
            code(ui, "Open Folder");
            ui.label(". Create a folder named");
            code(ui, "chrome");
            ui.label("in it, if there is none.");
        });
        if ui.button("Open about:support").clicked() {
            acts.open_page("about:support");
        }
        ui.add_space(6.0);

        step(ui, "3", "Add the stylesheet");
        ui.horizontal_wrapped(|ui| {
            ui.label("Save this as");
            code(ui, "chrome/userChrome.css");
            ui.label("(or add it to the end of the file you have):");
        });
        egui::Frame::new()
            .fill(p.input)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(&css).monospace()).selectable(true));
            });
        ui.horizontal(|ui| {
            if ui.button("Copy CSS").clicked() {
                ui.ctx().copy_text(css.clone());
                acts.notice("userChrome.css copied");
            }
            if ui.button("Download userChrome.css").on_hover_text("Saved to your downloads; move it into the chrome folder").clicked() {
                match pf::download_text("userChrome.css", &css, "text/css") {
                    Ok(()) => acts.notice("Downloaded userChrome.css: move it into your profile's chrome folder"),
                    Err(e) => acts.notice(format!("Could not download: {e}")),
                }
            }
        });
        ui.add_space(6.0);

        step(ui, "4", "Restart Firefox");
        ui.label(
            RichText::new(
                "To bring the tab bar back, delete those lines (or set the preference back to false) and restart. \
                     If the window buttons disappear with the tab bar, turn on Title Bar in Customize Toolbar….",
            )
            .small()
            .color(p.muted),
        );
    });
}

fn step(ui: &mut egui::Ui, n: &str, title: &str) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.label(RichText::new(n).strong().color(p.accent_hover));
        ui.label(RichText::new(title).strong());
    });
}

fn code(ui: &mut egui::Ui, text: &str) {
    let p = theme::palette(ui);
    ui.add(egui::Label::new(RichText::new(text).monospace().background_color(p.surface_hover)).selectable(true));
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// A one-line text field whose text is kept while it's edited (the settings are copied
/// from the state every frame). Returns the text when editing ends with a change.
fn draft_line(ui: &mut egui::Ui, id: &str, current: &str, hint: &str, width: f32) -> Option<String> {
    let key = egui::Id::unique(("settings-draft", id));
    let mut text: String = ui.data_mut(|d| d.get_temp::<String>(key)).unwrap_or_else(|| current.to_string());
    let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(hint).desired_width(width));
    if r.has_focus() {
        ui.data_mut(|d| d.insert_temp(key, text.clone()));
        return None;
    }
    ui.data_mut(|d| d.remove::<String>(key));
    (r.lost_focus() && text != current).then_some(text)
}

/// Settings of the AI integrations: providers, summaries and vector search.
fn ai_card(ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions, s: &mut crate::model::Settings, changed: &mut bool) {
    use crate::ai::{AiProvider, ProviderKind};
    let p = theme::palette(ui);
    section_card(
        ui,
        "AI & search",
        "Summaries of your tabs' pages by a language model you choose, saved as each tab's description, and search by meaning.",
        |ui| {
            let ai = &mut s.ai;
            *changed |= setting(
                ui,
                &mut ai.enabled,
                "Use an AI provider",
                "Off: nothing is ever sent. On: the text of the pages you summarise is sent to the provider below.",
            );
            ui.add_enabled_ui(ai.enabled, |ui| {
                let draft_key = egui::Id::unique("ai-provider-draft");
                let mut draft: Option<AiProvider> = ui.data_mut(|d| d.get_temp(draft_key));
                let selected = ai.active_provider().map(|x| x.id);
                let mut remove = None;
                for prov in &ai.providers {
                    ui.horizontal(|ui| {
                        if ui.radio(selected == Some(prov.id), "").on_hover_text("Use this provider").clicked() && selected != Some(prov.id) {
                            ai.provider = prov.id;
                            *changed = true;
                        }
                        ui.label(RichText::new(&prov.name).strong());
                        let model = if prov.model.is_empty() { "no model" } else { &prov.model };
                        ui.label(RichText::new(format!("{} · {model}", prov.kind.label())).small().color(p.muted));
                        if ui.small_button("✏").on_hover_text("Edit").clicked() {
                            draft = Some(prov.clone());
                        }
                        if ui.small_button("🗑").on_hover_text("Remove (summaries already written stay)").clicked() {
                            remove = Some(prov.id);
                        }
                    });
                }
                if let Some(id) = remove {
                    ai.providers.retain(|x| x.id != id);
                    *changed = true;
                }
                if ai.providers.is_empty() {
                    note(
                        ui,
                        "No provider yet. OpenAI-compatible covers OpenAI, Ollama (http://localhost:11434/v1), LM Studio, OpenRouter and others.",
                    );
                }
                if draft.is_none() && ui.button("＋ Add a provider").clicked() {
                    let kind = ProviderKind::OpenAiCompatible;
                    draft = Some(AiProvider { name: "My provider".into(), kind, base_url: kind.default_base_url().into(), ..Default::default() });
                }
                let mut close = false;
                if let Some(d) = &mut draft {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        egui::Grid::new("ai-provider-form").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.text_edit_singleline(&mut d.name);
                            ui.end_row();
                            ui.label("Kind");
                            egui::ComboBox::from_id_salt("ai-kind").selected_text(d.kind.label()).show_ui(ui, |ui| {
                                for k in ProviderKind::ALL {
                                    if ui.selectable_label(d.kind == k, k.label()).clicked() && d.kind != k {
                                        d.kind = k;
                                        d.base_url = k.default_base_url().into();
                                        if d.model.is_empty() {
                                            d.model = k.default_model().into();
                                        }
                                    }
                                }
                            });
                            ui.end_row();
                            ui.label("Base URL");
                            ui.add(egui::TextEdit::singleline(&mut d.base_url).desired_width(320.0));
                            ui.end_row();
                            ui.label("API key");
                            ui.add(egui::TextEdit::singleline(&mut d.api_key).password(true).hint_text("empty for local servers").desired_width(320.0));
                            ui.end_row();
                            ui.label("Model");
                            ui.add(
                                egui::TextEdit::singleline(&mut d.model)
                                    .hint_text("e.g. llama3.2, gpt-4o-mini, claude-opus-5-5")
                                    .desired_width(320.0),
                            );
                            ui.end_row();
                            ui.label("Embedding model");
                            ui.add_enabled_ui(d.kind == ProviderKind::OpenAiCompatible, |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut d.embedding_model)
                                        .hint_text("optional, e.g. nomic-embed-text; empty: local vectors")
                                        .desired_width(320.0),
                                );
                            });
                            ui.end_row();
                            ui.label("Instructions");
                            ui.add(egui::TextEdit::multiline(&mut d.prompt).hint_text(crate::ai::DEFAULT_PROMPT).desired_rows(2).desired_width(320.0));
                            ui.end_row();
                        });
                        ui.label(RichText::new("The API key is kept in the extension's local storage on this computer.").small().color(p.muted));
                        ui.horizontal(|ui| {
                            if ui.add(super::widgets::primary_button(ui, "Save provider")).clicked() {
                                let mut d = d.clone();
                                if d.id == 0 {
                                    d.id = ai.providers.iter().map(|x| x.id).max().unwrap_or(0) + 1;
                                    if ai.providers.is_empty() {
                                        ai.provider = d.id;
                                    }
                                    ai.providers.push(d);
                                } else if let Some(x) = ai.providers.iter_mut().find(|x| x.id == d.id) {
                                    *x = d;
                                }
                                *changed = true;
                                close = true;
                            }
                            if ui.button("Cancel").clicked() {
                                close = true;
                            }
                        });
                    });
                }
                if close {
                    draft = None;
                }
                ui.data_mut(|data| match draft {
                    Some(d) => {
                        data.insert_temp(draft_key, d);
                    }
                    None => data.remove::<AiProvider>(draft_key),
                });

                ui.add_space(6.0);
                *changed |= setting(
                    ui,
                    &mut ai.auto_summarize,
                    "Summarise pages automatically",
                    "A few seconds after a tab opens a page, its text is sent for a summary. Off: only when you ask \
                     (selected tabs → ✨ Summarise).",
                );
                ui.horizontal(|ui| {
                    ui.label("Never send pages of");
                    if let Some(text) = draft_line(ui, "ai-exclude", &ai.exclude_sites.join(", "), "bank.com, mail.example.org", 300.0) {
                        ai.exclude_sites = text.split([',', ' ', '\n']).map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
                        *changed = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Send at most");
                    *changed |= ui.add(egui::DragValue::new(&mut ai.max_input_chars).range(500..=200_000).speed(100)).changed();
                    ui.label(RichText::new("characters of each page").color(p.muted));
                });
            });
            ui.separator();
            *changed |= setting(
                ui,
                &mut ai.vector_search,
                "Search by meaning (vector search)",
                "Keeps a vector index of tab titles, sites and summaries, saved per session. Uses the provider's embedding model if it has \
                 one, otherwise vectors computed here (nothing sent). Off: the index is kept but not used or updated.",
            );
            ui.add_enabled_ui(ai.vector_search, |ui| {
                *changed |= setting(
                    ui,
                    &mut ai.unload_inactive,
                    "Free other sessions' search data after searching",
                    "After a search across sessions, the indexes of the inactive sessions leave memory (they're read again when needed).",
                );
                if ui.button("Free search memory now").on_hover_text("Unload the vector indexes of the inactive sessions").clicked() {
                    acts.send(Command::FreeSearchMemory);
                }
            });
            let described = cx.state.sessions.iter().flat_map(|x| &x.entries).filter(|e| !e.description.is_empty()).count();
            ui.label(RichText::new(format!("{described} tab(s) have a summary.")).small().color(p.muted));
        },
    );
}
