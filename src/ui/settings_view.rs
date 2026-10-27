//! Settings view.

use eframe::egui::{self, RichText};

use super::theme;
use super::widgets::section_card;
use super::{Actions, ViewCtx};
use crate::media::MediaPolicy;
use crate::platform as pf;
use crate::protocol::Command;
use crate::tab_strip::{FIREFOX_EXTENSION_ID, USER_CHROME_PREF, sidebar_command_id, user_chrome_css};

pub fn show(ui: &mut egui::Ui, cx: &ViewCtx, acts: &mut Actions) {
    let p = theme::palette(ui);
    let mut s = cx.state.settings.clone();
    let mut changed = false;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
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

        section_card(ui, "Workspaces in the browser", "Show your workspaces in the browser's own tab bar, sidebar and window title.", |ui| {
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
        });

        section_card(
            ui,
            "Media playback",
            "Pause videos playing in background tabs, so only the tab you're looking at plays.",
            |ui| {
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
                                egui::ComboBox::from_id_salt("max-background")
                                    .selected_text(m.max_background.to_string())
                                    .width(50.0)
                                    .show_ui(ui, |ui| {
                                        for n in [1, 2, 3, 4, 5, 10] {
                                            changed |= ui.selectable_value(&mut m.max_background, n, n.to_string()).changed();
                                        }
                                    });
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
                    changed |= setting(
                        ui,
                        &mut m.allow_pip,
                        "Keep picture-in-picture playing",
                        "A video shown picture-in-picture counts as visible.",
                    );
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
            },
        );

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
    egui::CollapsingHeader::new(RichText::new("Hide Firefox's tab bar (userChrome.css)").strong())
        .id_salt("user-chrome")
        .show(ui, |ui| {
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
