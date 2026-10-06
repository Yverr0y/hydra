// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! "Enter new address to download" — Add URL dialog.

use crate::app::{App, El, Message, WinKind};
use crate::windows::{check, dlg_btn_sized};
use crate::{i18n::tr, theme};
use iced::widget::{column, container, pick_list, row, text, text_input};
use iced::Length;

/// Widget id of the Address box, so the dialog can open with the caret in
/// it (`App::update`, `Message::WindowOpened`).
pub const ADDRESS_ID: &str = "add-url-address";

/// The label column, matching the Download File Info dialog's. A floor:
/// the column is drawn as wide as the widest label this locale produced,
/// since a label that outgrows it would be painted over its field.
const LABEL_W: f32 = 70.0;
const GAP: f32 = 8.0;

/// Every label in the column, so one width fits all of them and the rows
/// stay aligned.
const LABELS: [&str; 10] = [
    "Address",
    "Cookies",
    "Stream",
    "Quality",
    "Record",
    "Metalink",
    "Video",
    "Audio",
    "Audio format",
    "Container",
];

fn label_w() -> f32 {
    LABELS
        .iter()
        .fold(LABEL_W, |w, l| crate::windows::label_width(&tr(l), w))
}

fn label<'a>(s: String) -> El<'a> {
    text(s)
        .size(theme::FONT_SIZE)
        .wrapping(iced::widget::text::Wrapping::None)
        .width(label_w())
        .into()
}

pub fn view(app: &App) -> El<'_> {
    let st = &app.add_url;

    // No in-window heading: the OS title bar already names the dialog, and
    // puts the Address box on the very first line.
    let address = row![
        label(tr("Address")),
        crate::windows::ext_hint(
            text_input("http://", &st.address)
                .padding([5, 8])
                .id(ADDRESS_ID)
                .on_input(Message::AddrChanged)
                .on_submit(Message::AddUrlOk)
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
            &st.address,
        ),
    ]
    .spacing(GAP)
    .height(28.0)
    .align_y(iced::Alignment::Center);

    let auth = check(st.use_auth, tr("Use authorization")).on_toggle(Message::AddrAuthToggled);

    // Login and Password stay on screen and grey out until the box is
    // ticked, the way draws them: the dialog keeps one shape instead of
    // growing a row under the pointer.
    let enabled = st.use_auth;
    let cred_label = |s: String| {
        text(s)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None)
            .style(move |t: &iced::Theme| iced::widget::text::Style {
                color: Some(if enabled {
                    theme::text_color(t)
                } else {
                    theme::dim_text(t)
                }),
            })
    };
    let creds = row![
        iced::widget::space::horizontal().width(label_w()),
        cred_label(tr("Login")),
        text_input("", &st.login)
            .on_input_maybe(st.use_auth.then_some(Message::AddrLogin))
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
        iced::widget::space::horizontal().width(16.0),
        cred_label(tr("Password")),
        text_input("", &st.password)
            .on_input_maybe(st.use_auth.then_some(Message::AddrPass))
            .secure(true)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill),
    ]
    .spacing(GAP)
    .align_y(iced::Alignment::Center);

    // Cookies sit with the credentials because they are one: a session is
    // what gets a file out of a university mirror or a private GitLab, and a
    // user who has one in hand has nowhere else in this dialog to put it.
    let cookie_note: El<'_> = if st.cookies_importing {
        text(tr("Reading the browser's cookies..."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light))
            .into()
    } else {
        match &st.cookie_note {
            Some(n) => row![
                iced::widget::space::horizontal().width(label_w()),
                text(n.clone())
                    .size(theme::FONT_SIZE - 1.0)
                    .color(theme::dim_text(&iced::Theme::Light)),
            ]
            .spacing(GAP)
            .into(),
            None => iced::widget::space::horizontal().height(0.0).into(),
        }
    };
    let cookies = row![
        label(tr("Cookies")),
        container(
            text_input(
                "name=value; name2=value2",
                st.capture.cookies.as_deref().unwrap_or("")
            )
            .padding([5, 8])
            .on_input(Message::AddrCookies)
            .size(theme::FONT_SIZE)
            .style(theme::input)
            .width(Length::Fill)
        )
        .width(Length::Fill)
        .height(28.0)
        .center_y(28.0),
    ]
    .spacing(GAP)
    .height(28.0)
    .align_y(iced::Alignment::Center);

    // A manifest is not a file: which rendition, which container, and — for
    // a live stream — how long to record all have to be settled BEFORE the
    // first segment, because there is no changing your mind halfway through.
    let stream: El<'_> = if st.stream_probing {
        text(tr("Reading the stream's quality list..."))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light))
            .into()
    } else if let Some(p) = &st.stream {
        if let Some(drm) = &p.drm {
            // The same sentence the engine reports, so the dialog and the
            // download list never explain this differently.
            text(crate::engine::drm_refusal(drm))
                .size(theme::FONT_SIZE)
                .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
                .into()
        } else {
            let mut rows = column![].spacing(8);
            let kind = if p.live {
                format!("{} — {}", p.protocol.to_uppercase(), tr("live"))
            } else {
                match p.duration {
                    Some(d) => format!(
                        "{}  {}",
                        p.protocol.to_uppercase(),
                        crate::fmt::eta(d as u64)
                    ),
                    None => p.protocol.to_uppercase(),
                }
            };
            rows = rows.push(
                row![
                    label(tr("Stream")),
                    text(kind).size(theme::FONT_SIZE),
                    text(if p.separate_audio {
                        tr("video + audio")
                    } else {
                        String::new()
                    })
                    .size(theme::FONT_SIZE - 1.0)
                    .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            rows = rows.push(
                row![
                    label(tr("Quality")),
                    // Styled like every other picker in the app. Without
                    // this it falls back to iced's default surface, which is
                    // near-white and unreadable under the dark theme — this
                    // was the one picker in the app that skipped it.
                    pick_list(
                        p.qualities.clone(),
                        st.quality.clone(),
                        Message::AddrQuality
                    )
                    .text_size(theme::FONT_SIZE)
                    .style(theme::picker)
                    .menu_style(theme::picker_menu)
                    .padding([5, 8])
                    .width(250.0),
                    text(tr("Save as")).size(theme::FONT_SIZE),
                    pick_list(
                        // Renaming fragmented MP4 to .ts would produce a file
                        // that lies about itself, so TS is only offered when
                        // the segments really are MPEG-TS.
                        if p.is_ts {
                            vec!["MP4".to_string(), "TS".to_string()]
                        } else {
                            vec!["MP4".to_string()]
                        },
                        Some(st.container.clone()),
                        Message::AddrContainer
                    )
                    .text_size(theme::FONT_SIZE)
                    .style(theme::picker)
                    .menu_style(theme::picker_menu)
                    .padding([5, 8])
                    .width(100.0),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
            if p.live {
                rows = rows.push(
                    row![
                        label(tr("Record")),
                        text_input("", &st.record_minutes)
                            .on_input(Message::AddrRecordMinutes)
                            .size(theme::FONT_SIZE)
                            .style(theme::input)
                            .width(70.0),
                        text(tr(
                            "minutes, then finish the file. Leave empty to record until you press Stop."
                        ))
                        .size(theme::FONT_SIZE - 1.0)
                        .color(theme::dim_text(&iced::Theme::Light)),
                    ]
                    .spacing(8)
                    .align_y(iced::Alignment::Center),
                );
            }
            // A blank line under the block, so the mirror list or an error
            // line below reads as its own section.
            rows = rows.push(iced::widget::space::vertical().height(GAP));
            rows.into()
        }
    } else {
        iced::widget::space::horizontal().height(0.0).into()
    };

    // What went wrong reading a manifest or a mirror list is shown, not
    // logged: the address will otherwise be downloaded as a file, and a
    // 6 KB playlist named like a video is a puzzle for the user to solve.
    let probe_error = st.stream_error.as_ref().or(st.metalink_error.as_ref());
    let error: El<'_> = match st.error.as_ref().or(probe_error) {
        Some(e) => iced::widget::scrollable(
            text(e.clone())
                .size(theme::FONT_SIZE)
                .color(theme::error_text()),
        )
        .height(error_panel_height(e))
        .into(),
        None if !st.address.trim().is_empty()
            && crate::app::site_blocked(st.address.trim(), &app.cfg.settings.dont_start_sites) =>
        {
            text(tr(
                "This site is on the auto-start block list; it will be added, not started.",
            ))
            .size(theme::FONT_SIZE)
            .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B))
            .into()
        }
        None => iced::widget::space::horizontal().height(0.0).into(),
    };

    // Nothing may be added while the address is still being read.
    let probing = st.stream_probing || st.metalink_probing || st.plugin_probing;
    let actions = crate::plugins::input_actions(app);
    let button_width = actions
        .iter()
        .map(|(_, _, action)| action.label.as_str())
        .chain([tr("OK"), tr("Cancel")].iter().map(String::as_str))
        .map(crate::windows::btn_width)
        .fold(0.0_f32, f32::max);
    let mut buttons = column![
        dlg_btn_sized(
            tr("OK"),
            (!probing).then_some(Message::AddUrlOk),
            button_width,
            true
        ),
        dlg_btn_sized(
            tr("Cancel"),
            app.win_of(WinKind::AddUrl).map(Message::CloseThis),
            button_width,
            false
        ),
    ]
    .spacing(8);

    for (id, index, action) in actions {
        buttons = buttons.push(dlg_btn_sized(
            action.label,
            (!probing).then_some(Message::PluginBrowse(id, index)),
            button_width,
            false,
        ));
    }

    // A mirror list is not a file either, and what it changes is worth seeing
    // before OK: how many files are about to be added, how many mirrors each
    // has, and whether it can be verified per chunk. A list that silently loses
    // two thirds of its entries to a scheme this build cannot fetch is worth
    // knowing about before a multi-gigabyte download rather than after.
    let metalink: El<'_> = if st.metalink_probing {
        text(tr("Reading the mirror list..."))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light))
            .into()
    } else if let Some(m) = &st.metalink {
        let mut rows = column![].spacing(6);
        rows = rows.push(
            row![
                label(tr("Metalink")),
                text(format!(
                    "{}  —  {} {}",
                    m.version,
                    m.files.len(),
                    if m.files.len() == 1 {
                        tr("file")
                    } else {
                        tr("files")
                    }
                ))
                .size(theme::FONT_SIZE),
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center),
        );
        // A summary, not a listing: a document with forty files would push the
        // OK button off the window. `metalink_panel_height` reserves space for
        // exactly this many rows plus the "+N" line, and the two must agree.
        for f in m.files.iter().take(crate::app::METALINK_PANEL_ROWS) {
            let usable = f.info.mirrors.len();
            let mut parts = vec![match f.info.size {
                Some(n) => crate::fmt::size2(n),
                None => tr("size not stated"),
            }];
            parts.push(format!("{usable}/{} {}", f.mirrors_listed, tr("mirrors")));
            if f.piece_count > 0 {
                parts.push(format!("{} {}", f.piece_count, tr("verified chunks")));
            } else if f.info.digest.is_some() {
                parts.push(tr("whole-file checksum"));
            } else {
                parts.push(tr("no checksum published"));
            }
            if f.info.signed {
                parts.push(tr("signed (not verified)"));
            }
            rows = rows.push(
                row![
                    iced::widget::space::horizontal().width(LABEL_W),
                    text(f.name.clone()).size(theme::FONT_SIZE),
                    text(parts.join("  ·  "))
                        .size(theme::FONT_SIZE - 1.0)
                        .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
            );
        }
        if m.files.len() > crate::app::METALINK_PANEL_ROWS {
            rows = rows.push(
                row![
                    iced::widget::space::horizontal().width(LABEL_W),
                    text(format!(
                        "+{}",
                        m.files.len() - crate::app::METALINK_PANEL_ROWS
                    ))
                    .size(theme::FONT_SIZE - 1.0)
                    .color(theme::dim_text(&iced::Theme::Light)),
                ]
                .spacing(8),
            );
        }
        rows.into()
    } else {
        iced::widget::space::horizontal().height(0.0).into()
    };

    let mut plugins = column![].spacing(GAP);
    if st.plugin_probing {
        plugins = plugins.push(text(tr("Reading plugin tracks…")).size(theme::FONT_SIZE));
    }
    if let Some(info) = &st.plugin_plan {
        if let Some(transfer) = &info.plan.transfer {
            let mut files = column![].spacing(4);
            let mut selected_count = 0;
            let mut selected_size = 0_u64;
            for file in &transfer.files {
                let index = file.index;
                let selected = info
                    .preferences
                    .transfer_files
                    .as_ref()
                    .is_none_or(|files| files.contains(&index));
                if selected {
                    selected_count += 1;
                    selected_size = selected_size.saturating_add(file.size);
                }
                files = files.push(
                    row![
                        check(selected, file.path.clone())
                            .on_toggle(move |selected| Message::TransferFileSelected(
                                index, selected
                            ))
                            .width(Length::Fill),
                        text(crate::fmt::size2(file.size)).size(theme::FONT_SIZE)
                    ]
                    .spacing(GAP)
                    .align_y(iced::Alignment::Center),
                );
            }
            let summary = format!(
                "{} / {} · {}",
                selected_count,
                transfer.files.len(),
                crate::fmt::size2(selected_size)
            );
            let content = column![
                iced::widget::scrollable(
                    text(
                        info.plan
                            .title
                            .clone()
                            .unwrap_or_else(|| info.plan.id.clone())
                    )
                    .size(theme::FONT_SIZE)
                    .width(Length::Fill)
                )
                .height(title_height(info)),
                row![
                    text(tr("Files")).size(theme::FONT_SIZE).width(Length::Fill),
                    text(summary).size(theme::FONT_SIZE - 1.0)
                ]
                .spacing(GAP),
                iced::widget::scrollable(files).height(list_height(
                    transfer.files.len(),
                    26.0,
                    160.0
                )),
            ]
            .spacing(GAP);
            plugins = plugins.push(
                container(content)
                    .padding(10)
                    .width(Length::Fill)
                    .style(theme::panel),
            );
            if let Some(notice) = &transfer.notice {
                plugins = plugins.push(
                    iced::widget::scrollable(text(notice.clone()).size(theme::FONT_SIZE - 1.0))
                        .height(32.0),
                );
            }
        } else {
            plugins = plugins.push(
                iced::widget::scrollable(
                    text(plugin_title(info))
                        .size(theme::FONT_SIZE)
                        .width(Length::Fill),
                )
                .height(title_height(info)),
            );
        }
        if info.plan.transfer.is_none() {
            plugins = plugins.push(
                row![
                    iced::widget::space::horizontal().width(label_w()),
                    check(info.preferences.audio_only, tr("Audio only"))
                        .on_toggle(Message::PluginAudioOnly),
                ]
                .spacing(GAP)
                .align_y(iced::Alignment::Center),
            );
        }
        if info.preferences.audio_only {
            plugins = plugins.push(
                row![
                    label(tr("Audio format")),
                    pick_list(
                        vec![
                            "Original".to_string(),
                            "mp3".into(),
                            "m4a".into(),
                            "opus".into(),
                            "flac".into(),
                            "wav".into()
                        ],
                        Some(
                            info.preferences
                                .audio_format
                                .clone()
                                .unwrap_or_else(|| "Original".into())
                        ),
                        Message::PluginAudioFormat
                    )
                    .text_size(theme::FONT_SIZE)
                    .padding([5, 8])
                    .style(theme::picker)
                    .menu_style(theme::picker_menu)
                    .width(Length::Fill),
                ]
                .spacing(GAP)
                .align_y(iced::Alignment::Center),
            );
        }
        if !info.plan.entries.is_empty() {
            let mut entries = column![].spacing(6);
            for entry in &info.plan.entries {
                let id = entry.id.clone();
                let checked = info
                    .preferences
                    .playlist_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&id));
                entries = entries.push(
                    check(
                        checked,
                        entry.title.clone().unwrap_or_else(|| entry.id.clone()),
                    )
                    .on_toggle(move |checked| Message::PluginPlaylistEntry(id.clone(), checked)),
                );
            }
            plugins = plugins.push(text(tr("Playlist items")).size(theme::FONT_SIZE));
            plugins = plugins.push(
                iced::widget::scrollable(entries)
                    .width(Length::Fill)
                    .height(list_height(info.plan.entries.len(), 24.0, 120.0)),
            );
            plugins = plugins.push(
                text(tr("Selected videos will be added to the main queue."))
                    .size(theme::FONT_SIZE - 1.0),
            );
            if !info.preferences.audio_only {
                plugins = plugins.push(
                    row![
                        label(tr("Quality")),
                        pick_list(
                            vec![
                                "Best".to_string(),
                                "2160p".into(),
                                "1440p".into(),
                                "1080p".into(),
                                "720p".into(),
                                "480p".into()
                            ],
                            Some(
                                info.preferences
                                    .max_height
                                    .map(|height| format!("{height}p"))
                                    .unwrap_or_else(|| "Best".into())
                            ),
                            |quality: String| Message::PluginMaxHeight(
                                quality.trim_end_matches('p').parse().ok()
                            )
                        )
                        .text_size(theme::FONT_SIZE)
                        .padding([5, 8])
                        .style(theme::picker)
                        .menu_style(theme::picker_menu)
                        .width(Length::Fill)
                    ]
                    .spacing(GAP),
                );
            }
        }
        for (kind, track_label) in [
            (hya_plugin_api::TrackKind::Video, "Video"),
            (hya_plugin_api::TrackKind::Audio, "Audio"),
        ] {
            if !info.plan.entries.is_empty()
                || (kind == hya_plugin_api::TrackKind::Video && info.preferences.audio_only)
            {
                continue;
            }
            if !info.plan.tracks.iter().any(|track| track.kind == kind) {
                continue;
            }
            let mut ids = vec!["Best".to_string()];
            if kind == hya_plugin_api::TrackKind::Audio {
                ids.push("None".into());
            }
            ids.extend(
                info.plan
                    .tracks
                    .iter()
                    .filter(|t| t.kind == kind)
                    .map(|t| t.id.clone()),
            );
            let selected = info
                .preferences
                .track_ids
                .iter()
                .find(|id| info.plan.track(id).is_some_and(|t| t.kind == kind))
                .cloned()
                .unwrap_or_else(|| {
                    if kind == hya_plugin_api::TrackKind::Audio
                        && info.preferences.audio == hya_plugin_api::AudioPref::None
                    {
                        "None".into()
                    } else {
                        "Best".into()
                    }
                });
            plugins = plugins.push(
                row![
                    label(tr(track_label)),
                    pick_list(
                        ids.iter()
                            .map(|id| TrackChoice::new(id, &info.plan))
                            .collect::<Vec<_>>(),
                        Some(TrackChoice::new(&selected, &info.plan)),
                        move |choice| Message::PluginTrack(kind, choice.id)
                    )
                    .text_size(theme::FONT_SIZE)
                    .padding([5, 8])
                    .style(theme::picker)
                    .menu_style(theme::picker_menu)
                    .width(Length::Fill)
                ]
                .spacing(GAP)
                .align_y(iced::Alignment::Center),
            );
        }
        let mut subtitles = column![].spacing(4);
        for track in info
            .plan
            .tracks
            .iter()
            .filter(|t| t.kind == hya_plugin_api::TrackKind::Subtitle)
        {
            let id = track.id.clone();
            subtitles = subtitles.push(
                check(
                    info.preferences.track_ids.contains(&id),
                    format!(
                        "{} · {}{}",
                        track.language.as_deref().unwrap_or(&id),
                        track.container.as_deref().unwrap_or(""),
                        if track.auto_generated { " (auto)" } else { "" }
                    ),
                )
                .on_toggle(move |value| Message::PluginSubtitle(id.clone(), value)),
            );
        }
        if info
            .plan
            .tracks
            .iter()
            .any(|track| track.kind == hya_plugin_api::TrackKind::Subtitle)
        {
            plugins = plugins.push(text(tr("Subtitles")).size(theme::FONT_SIZE));
            plugins = plugins.push(
                iced::widget::scrollable(subtitles)
                    .width(Length::Fill)
                    .height(list_height(subtitle_count(info), 22.0, 90.0)),
            );
        }
        if !info.preferences.audio_only && info.plan.transfer.is_none() {
            plugins = plugins.push(
                row![
                    label(tr("Container")),
                    pick_list(
                        vec!["mp4".to_string(), "mkv".to_string(), "webm".to_string()],
                        info.preferences.container.clone(),
                        Message::PluginContainer
                    )
                    .text_size(theme::FONT_SIZE)
                    .padding([5, 8])
                    .style(theme::picker)
                    .menu_style(theme::picker_menu)
                    .width(Length::Fill)
                ]
                .spacing(GAP)
                .align_y(iced::Alignment::Center),
            );
        }
    }

    let mut left = column![address, auth, creds, cookies]
        .spacing(GAP)
        .width(Length::Fill);
    if st.cookies_importing || st.cookie_note.is_some() {
        left = left.push(cookie_note);
    }
    if st.stream_probing || st.stream.is_some() {
        left = left.push(stream);
    }
    if st.metalink_probing || st.metalink.is_some() {
        left = left.push(metalink);
    }
    if st.plugin_probing || st.plugin_plan.is_some() {
        left = left.push(plugins);
    }
    if st.error.is_some()
        || probe_error.is_some()
        || (!st.address.trim().is_empty()
            && crate::app::site_blocked(st.address.trim(), &app.cfg.settings.dont_start_sites))
    {
        left = left.push(error);
    }

    // OK over Cancel on the right, level with the Address box.
    container(row![left, buttons].spacing(16).padding(12))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

fn plugin_title(info: &crate::plugins::PlanInfo) -> String {
    format!(
        "{}: {}",
        info.plugin,
        info.plan.title.as_deref().unwrap_or(&info.plan.id)
    )
}

fn title_height(info: &crate::plugins::PlanInfo) -> f32 {
    if crate::font::line_width(&plugin_title(info), theme::FONT_SIZE) > 580.0 {
        34.0
    } else {
        17.0
    }
}

fn list_height(count: usize, row_height: f32, maximum: f32) -> f32 {
    (count as f32 * row_height).min(maximum)
}

/// Reserves space for wrapped diagnostics while bounding the dialog height.
pub(crate) fn error_panel_height(error: &str) -> f32 {
    let lines: usize = error
        .lines()
        .map(|line| line.chars().count().div_ceil(70).max(1))
        .sum();
    (lines as f32 * 20.0).clamp(40.0, 120.0)
}

fn subtitle_count(info: &crate::plugins::PlanInfo) -> usize {
    info.plan
        .tracks
        .iter()
        .filter(|track| track.kind == hya_plugin_api::TrackKind::Subtitle)
        .count()
}

pub(crate) fn plugin_panel_height(st: &crate::app::AddUrlState) -> f32 {
    let mut height = if st.plugin_probing { 25.0 } else { 0.0 };
    let Some(info) = &st.plugin_plan else {
        return height;
    };
    height += GAP + title_height(info) + GAP + 18.0;
    if let Some(transfer) = &info.plan.transfer {
        return height + list_height(transfer.files.len(), 26.0, 160.0) + 84.0;
    }
    if info.preferences.audio_only {
        height += 36.0;
    }
    if !info.plan.entries.is_empty() {
        height += 25.0 + list_height(info.plan.entries.len(), 24.0, 120.0) + GAP + 23.0;
        if !info.preferences.audio_only {
            height += 36.0;
        }
    } else {
        for kind in [
            hya_plugin_api::TrackKind::Video,
            hya_plugin_api::TrackKind::Audio,
        ] {
            if !(kind == hya_plugin_api::TrackKind::Video && info.preferences.audio_only)
                && info.plan.tracks.iter().any(|track| track.kind == kind)
            {
                height += 36.0;
            }
        }
    }
    let subtitles = subtitle_count(info);
    if subtitles > 0 {
        height += 25.0 + list_height(subtitles, 22.0, 90.0) + GAP;
    }
    if !info.preferences.audio_only {
        height += 36.0;
    }
    height
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrackChoice {
    id: String,
    label: String,
}
impl TrackChoice {
    fn new(id: &str, plan: &hya_plugin_api::Plan) -> Self {
        let label = plan
            .track(id)
            .map(|track| {
                let mut parts = vec![track.id.clone()];
                if let Some(height) = track.height {
                    parts.push(format!("{height}p"));
                }
                if let Some(codec) = &track.codec {
                    parts.push(codec.clone());
                }
                if let Some(container) = &track.container {
                    parts.push(container.clone());
                }
                if let Some(language) = &track.language {
                    parts.push(language.clone());
                }
                parts.join(" · ")
            })
            .unwrap_or_else(|| tr(id));
        Self {
            id: id.into(),
            label,
        }
    }
}
impl std::fmt::Display for TrackChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
