// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The download-progress window: Download status / Speed Limiter / Options on
//! completion tabs, overall progress bar, the "start positions and download
//! progress by connections" strip, and the per-connection table.

use crate::app::{App, El, Message, ProgTab, ScanState};
use crate::model::{DlState, DownloadItem, PowerAction, ProxyPick};
use crate::windows::{cell, check, dlg_btn, dlg_btn_primary};
use crate::{fmt, i18n::tr, theme};
use iced::widget::{
    button, column, container, pick_list, progress_bar, radio, row, text, text_input,
};
use iced::Length;

fn tab_btn<'a>(label: String, selected: bool, msg: Message) -> El<'a> {
    button(text(label).size(theme::FONT_SIZE))
        .padding([4, 12])
        .style(theme::btn_tab(selected))
        .on_press(msg)
        .into()
}

pub const WIDTH: f32 = 680.0;
pub const HEIGHT_DETAILS: f32 = 582.0;
pub const HEIGHT_COLLAPSED: f32 = 352.0;

pub fn standard_size(details: bool) -> (f32, f32) {
    (
        WIDTH,
        if details {
            HEIGHT_DETAILS
        } else {
            HEIGHT_COLLAPSED
        },
    )
}

/// The narrowest the status rows' key column is drawn; it grows to the
/// widest key the locale produced (see `crate::windows::label_column`).
const KEY_W: f32 = 130.0;
/// Floors for the two label columns that stand outside the status rows.
const PROXY_LABEL_W: f32 = 170.0;
const SAVE_TO_W: f32 = 70.0;
const ROW_GAP: f32 = 8.0;

/// One reported value, as a row for [`rows`].
fn kv<'a>(k: String, v: String) -> (Option<String>, El<'a>) {
    (Some(k), text(v).size(theme::FONT_SIZE).into())
}

/// The reported values of a tab, over one key column.
fn rows<'a>(rows: Vec<(Option<String>, El<'a>)>) -> El<'a> {
    crate::windows::label_column(rows, KEY_W, ROW_GAP)
}

fn status_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let recording = d.stream.as_ref().is_some_and(|s| s.live);
    let pct = d
        .size
        .map(|s| fmt::pct(d.downloaded, s))
        .unwrap_or_default();
    let status = (
        Some(tr("Status")),
        text(if d.status_line.is_empty() {
            d.status_text()
        } else {
            d.status_line.clone()
        })
        .size(theme::FONT_SIZE)
        // A virus verdict is the one status worth shouting: red for
        // "infected" or a scanner that could not run, the usual blue for
        // everything else, scanning included.
        .color(match app.scans.get(&d.id) {
            Some(st) if !st.running() => iced::Color::from_rgb8(0xC4, 0x1F, 0x1F),
            _ => iced::Color::from_rgb8(0x1F, 0x3F, 0xC4),
        })
        .into(),
    );
    // A live recording has no size and never will: it ends when someone
    // stops it. Reporting "?" against a label that expects a number says
    // nothing, so the row becomes the one measure that IS knowable —
    // how much media is on disk.
    let size_row = if recording {
        let done = d
            .recorded_secs
            .map(|s| fmt::eta(s as u64))
            .unwrap_or_else(|| "0:00".into());
        kv(
            tr("Recorded"),
            match d.stream.as_ref().and_then(|s| s.max_seconds) {
                // Asked for a fixed length: say how much of it is done.
                Some(limit) => format!("{done} / {}", fmt::eta(limit)),
                None => done,
            },
        )
    } else {
        kv(
            tr("File size"),
            d.size.map(fmt::size3).unwrap_or_else(|| "?".into()),
        )
    };
    column![
        crate::windows::ext_hint(
            text(d.url.clone())
                .size(theme::FONT_SIZE - 1.0)
                .wrapping(iced::widget::text::Wrapping::None),
            &d.url,
        ),
        rows(vec![
            status,
            size_row,
            kv(
                tr("Downloaded"),
                format!(
                    "{}{}",
                    fmt::size3(d.downloaded),
                    if pct.is_empty() {
                        String::new()
                    } else {
                        format!("  ( {pct} )")
                    }
                ),
            ),
            kv(
                tr("Transfer rate"),
                fmt::rate_capped(d.rate, app.effective_limit(d)),
            ),
            kv(
                tr("Time left"),
                d.eta_secs.map(fmt::eta).unwrap_or_default(),
            ),
            kv(
                tr("Resume capability"),
                match d.resume {
                    Some(true) => tr("Yes"),
                    Some(false) => tr("No"),
                    None => "?".into(),
                },
            ),
        ]),
    ]
    .spacing(7)
    .into()
}

/// The route this download takes, changeable here while it is stopped.
///
/// Only while it is stopped: a running transfer holds connections that were
/// opened through the proxy in force when it started, and moving an open
/// socket to another proxy is not something TCP offers. Pause and Start apply
/// the new route to the same bytes, which is why the tab says which button to
/// press rather than greying the row out with no explanation.
fn proxy_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let p = app.prog.get(&d.id).cloned().unwrap_or_default();
    let running = d.state.is_active();
    let id = d.id;
    let title = tr("Proxy for this download");
    let label = text(title.clone())
        .size(theme::FONT_SIZE)
        .wrapping(iced::widget::text::Wrapping::None)
        .width(crate::windows::label_width(&title, PROXY_LABEL_W));
    // While it runs the row REPORTS instead of offering: a picker that
    // accepts a click it cannot act on is the same broken promise as a
    // setting that never reaches the transport.
    let mut r = if running {
        row![
            label,
            text(match p.proxy_pick {
                ProxyPick::Custom => p.proxy_spec.clone(),
                other => other.to_string(),
            })
            .size(theme::FONT_SIZE),
        ]
    } else {
        row![
            label,
            pick_list(&ProxyPick::ALL[..], Some(p.proxy_pick), move |v| {
                Message::ProgProxyPick(id, v)
            })
            .text_size(theme::FONT_SIZE)
            .style(theme::picker)
            .width(170.0),
        ]
    }
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if !running && p.proxy_pick == ProxyPick::Custom {
        r = r.push(
            text_input("socks5://127.0.0.1:10808", &p.proxy_spec)
                .on_input(move |v| Message::ProgProxySpec(id, v))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(Length::Fill),
        );
    }
    let mut col = column![r].spacing(6);
    if let Some(why) = proxy_problem(&p).filter(|_| !running) {
        col = col.push(
            text(why)
                .size(theme::FONT_SIZE - 1.0)
                .color(iced::Color::from_rgb8(0xC0, 0x2B, 0x2B)),
        );
    }
    // What "Default" actually resolves to right now. A proxy tab that only
    // repeats the word "default" cannot answer the question people arrive
    // here with — whether the app can see the proxy they configured.
    if p.proxy_pick == ProxyPick::Default {
        col = col.push(
            text(
                tr("Options > Proxy/Socks currently uses: {route}")
                    .replace("{route}", &crate::proxy::active().describe()),
            )
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
        );
    }
    col = col.push(
        text(if running {
            tr(
                "Pause the download to change its proxy; Start applies the new route to the \
                bytes already on disk.",
            )
        } else {
            tr(
                "A SOCKS proxy carries every download; an HTTP proxy carries HTTP and HTTPS, \
                but not FTP.",
            )
        })
        .size(theme::FONT_SIZE - 1.0)
        .color(theme::dim_text(&iced::Theme::Light)),
    );
    col.into()
}

/// Why the address in the row is not a proxy, or `None` when it is one, when
/// none is being asked for, or when the box is still empty — an address not
/// yet typed is not a mistake to point at in red.
fn proxy_problem(p: &crate::app::ProgState) -> Option<String> {
    crate::proxy::typed_spec_error(p.proxy_pick, &p.proxy_spec)
}

fn speed_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let p = app.prog.get(&d.id).cloned().unwrap_or_default();
    column![
        rows(vec![kv(
            tr("Transfer rate"),
            fmt::rate_capped(d.rate, app.effective_limit(d)),
        )]),
        check(p.limit_on, tr("Use Speed Limiter"))
            .on_toggle(move |b| Message::ProgLimitOn(d.id, b)),
        text(tr("Maximum download speed:"))
            .size(theme::FONT_SIZE)
            .color(theme::dim_text(&iced::Theme::Light)),
        row![
            text_input("10", &p.limit_kb)
                .on_input(move |s| Message::ProgLimitKb(d.id, s))
                .size(theme::FONT_SIZE)
                .style(theme::input)
                .width(120.0),
            text(tr("KBytes/sec")).size(theme::FONT_SIZE),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        check(
            p.remember_limit,
            tr("Remember Speed Limiter settings for this file on download stop/resume")
        )
        .on_toggle(move |b| Message::ProgLimitRemember(d.id, b)),
        if app.cfg.settings.show_hide_buttons {
            crate::windows::dlg_btn(tr("Hide tab"), Some(Message::ProgHideTab(d.id, 1)))
        } else {
            iced::widget::space::horizontal().height(0.0).into()
        },
    ]
    .spacing(10)
    .into()
}

fn completion_tab<'a>(app: &'a App, d: &'a DownloadItem) -> El<'a> {
    let mut col = column![
        row![
            text(tr("Save To:"))
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::None)
                .width(crate::windows::label_width(&tr("Save To:"), SAVE_TO_W)),
            text(d.full_path().to_string_lossy().into_owned()).size(theme::FONT_SIZE),
        ]
        .spacing(8),
        check(
            app.cfg.settings.show_complete_dialog,
            tr("Show download complete dialog")
        )
        .on_toggle(Message::ProgShowCompleteDialog),
        check(
            app.cfg.settings.remove_completed,
            tr("Remove completed downloads from the list")
        )
        .on_toggle(Message::ProgRemoveCompleted),
        check(
            d.shutdown_after,
            tr("Shut down / log off / sleep computer when done")
        )
        .on_toggle(move |b| Message::ProgShutdownAfter(d.id, b)),
    ]
    .spacing(10);

    if d.shutdown_after {
        col = col.push(
            row![
                radio(
                    tr("Shutdown"),
                    PowerAction::Shutdown,
                    Some(d.shutdown_action),
                    move |v| Message::ProgShutdownAction(d.id, v),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
                radio(
                    tr("Log off"),
                    PowerAction::LogOff,
                    Some(d.shutdown_action),
                    move |v| Message::ProgShutdownAction(d.id, v),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
                radio(
                    tr("Sleep"),
                    PowerAction::Sleep,
                    Some(d.shutdown_action),
                    move |v| Message::ProgShutdownAction(d.id, v),
                )
                .size(15.0)
                .text_size(theme::FONT_SIZE),
            ]
            .spacing(30),
        );
    }

    if app.cfg.settings.show_hide_buttons {
        col = col.push(crate::windows::dlg_btn(
            tr("Hide tab"),
            Some(Message::ProgHideTab(d.id, 2)),
        ));
    }

    col.into()
}

/// The indeterminate bar shown while the virus scanner works: iced's
/// `progress_bar` is determinate only, so the moving block is a FillPortion
/// row inside the same track the real bar draws.
fn marquee<'a>(sweep: f32) -> El<'a> {
    const TOTAL: u16 = 1000;
    const BLOCK: u16 = 220;
    let travel = f32::from(TOTAL - BLOCK);
    // Both spacers keep a portion: FillPortion(0) would divide the row by a
    // zero total.
    let left = (sweep.clamp(0.0, 1.0) * travel)
        .round()
        .clamp(1.0, travel - 1.0) as u16;
    let right = (TOTAL - BLOCK).saturating_sub(left).max(1);
    let bar = row![
        iced::widget::space::horizontal().width(Length::FillPortion(left)),
        container(iced::widget::space::horizontal())
            .width(Length::FillPortion(BLOCK))
            .height(Length::Fill)
            .style(theme::scan_block),
        iced::widget::space::horizontal().width(Length::FillPortion(right)),
    ]
    .height(Length::Fill);
    container(bar)
        .width(Length::Fill)
        .height(18.0)
        .style(theme::scan_track)
        .into()
}

/// The band behind the connection rows and the scanner log. It fills the
/// whole viewport, so a window grown past the padded rows stays one band.
fn row_band(t: &iced::Theme) -> container::Style {
    container::Style {
        background: Some(iced::Background::Color(if theme::is_dark(t) {
            iced::Color::from_rgb8(0x33, 0x2E, 0x38)
        } else {
            iced::Color::from_rgb8(0xC9, 0xBF, 0xC9)
        })),
        text_color: Some(theme::text_color(t)),
        ..Default::default()
    }
}

/// The scanner's console output, in the box the per-connection table uses.
/// Anchored to the bottom so the newest line is the one on screen.
fn scan_log(st: &ScanState) -> El<'_> {
    const ROW_H: f32 = 20.0;
    // The connections box is a header plus 8 rows; the log has no header, so
    // it takes the ninth row instead and the panel keeps the same footprint.
    const VISIBLE: usize = 9;
    let mut rows = column![].width(Length::Fill);
    let n_rows = st.log.len().max(VISIBLE);
    for i in 0..n_rows {
        let line = st.log.get(i).cloned().unwrap_or_default();
        rows = rows.push(
            container(
                container(
                    text(line)
                        .size(theme::FONT_SIZE)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .padding([1, 6])
                .height(ROW_H),
            )
            .width(Length::Fill),
        );
    }
    container(
        container(
            crate::ui::scroll(rows)
                .height(Length::Fill)
                .width(Length::Fill)
                .anchor_bottom(),
        )
        .height(Length::Fill)
        .style(row_band),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::panel)
    .into()
}

/// The blue chunk strip: 120 buckets over the object, filled where held.
fn chunk_strip<'a>(d: &DownloadItem) -> El<'a> {
    const BUCKETS: usize = 120;
    let mut filled = [false; BUCKETS];
    if let Some(size) = d.size.filter(|s| *s > 0) {
        for &(lo, hi) in &d.held {
            let a = (lo as f64 / size as f64 * BUCKETS as f64) as usize;
            let b = ((hi as f64 / size as f64 * BUCKETS as f64).ceil() as usize).min(BUCKETS);
            for f in filled.iter_mut().take(b).skip(a) {
                *f = true;
            }
        }
    }
    let mut r = row![].spacing(0).width(Length::Fill).height(10.0);
    for on in filled {
        r = r.push(
            container(iced::widget::space::horizontal())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(if on {
                    theme::chunk_on
                } else {
                    theme::chunk_off
                }),
        );
    }
    container(r)
        .width(Length::Fill)
        .padding(1)
        .style(theme::panel)
        .into()
}

/// The two fixed columns of the connections table; the header and the rows
/// share the numbers.
const CONN_N_W: f32 = 50.0;
const CONN_SIZE_W: f32 = 150.0;

fn conn_table<'a>(d: &'a DownloadItem) -> El<'a> {
    // Header stays put; only the rows scroll.
    let header = row![
        cell(tr("N."), CONN_N_W).padding([2, 6]),
        cell(tr("Downloaded"), CONN_SIZE_W).padding([2, 6]),
        cell(tr("Info"), Length::Fill).padding([2, 6]),
    ]
    .spacing(0);
    // Blank rows pad shorter transfers to 8 so the box looks identical for 4
    // vs 8 connections; more connections scroll inside the viewport.
    const ROW_H: f32 = 20.0;
    const VISIBLE: usize = 8;
    let mut rows = column![].width(Length::Fill);
    let n_rows = d.conns.len().max(VISIBLE);
    let blank = crate::model::ConnRow::default();
    for i in 0..n_rows {
        let c = d.conns.get(i).unwrap_or(&blank);
        rows = rows.push(container(
            row![
                cell(format!("{}", i + 1), CONN_N_W).padding([1, 6]),
                cell(
                    if c.downloaded > 0 {
                        fmt::size3(c.downloaded)
                    } else {
                        String::new()
                    },
                    CONN_SIZE_W,
                )
                .padding([1, 6]),
                cell(c.info.clone(), Length::Fill).padding([1, 6]),
            ]
            .spacing(0)
            .height(ROW_H),
        ));
    }
    // The viewport takes whatever height the window leaves it: the default
    // window is sized for 8 rows, a resized one shows more.
    container(column![
        header,
        container(
            crate::ui::scroll(rows)
                .height(Length::Fill)
                .width(Length::Fill)
        )
        .height(Length::Fill)
        .style(row_band),
    ])
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::panel)
    .into()
}

pub fn view(app: &App, id: crate::model::DlId) -> El<'_> {
    let Some(d) = app.item(id) else {
        return container(text(""))
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::window)
            .into();
    };
    let p = app.prog.get(&id).cloned().unwrap_or_default();
    let s = &app.cfg.settings;

    let mut tabs = row![tab_btn(
        tr("Download status"),
        p.tab == ProgTab::Status,
        Message::ProgTabSet(id, ProgTab::Status),
    )]
    .spacing(1);
    if s.show_speed_tab {
        tabs = tabs.push(tab_btn(
            tr("Speed Limiter"),
            p.tab == ProgTab::Speed,
            Message::ProgTabSet(id, ProgTab::Speed),
        ));
    }
    // No hide-tab switch for this one: the two that have one are the tabs
    // IDM lets you dismiss, and a route the user cannot find is how the
    // proxy came to look broken in the first place.
    tabs = tabs.push(tab_btn(
        tr("Proxy"),
        p.tab == ProgTab::Proxy,
        Message::ProgTabSet(id, ProgTab::Proxy),
    ));
    if s.show_completion_tab {
        tabs = tabs.push(tab_btn(
            tr("Options on completion"),
            p.tab == ProgTab::Completion,
            Message::ProgTabSet(id, ProgTab::Completion),
        ));
    }

    let tab_body: El<'_> = match p.tab {
        ProgTab::Status => status_tab(app, d),
        ProgTab::Speed => speed_tab(app, d),
        ProgTab::Proxy => proxy_tab(app, d),
        ProgTab::Completion => completion_tab(app, d),
    };

    let active = d.state.is_active();
    // A segment stream is not paused, it is stopped: the segments already
    // fetched are kept and a live recording is finished into its file, so
    // the button says what it does. Pause stays for a byte-range download,
    // which really does pick up where it left off.
    let pause_label = if !active {
        tr("Start")
    } else if d.stream.is_some() {
        tr("Stop")
    } else {
        tr("Pause")
    };
    let scan = app.scans.get(&id);
    let scanning = scan.map(ScanState::running).unwrap_or(false);
    // A verdict still on screen: the scanner found something (or could not
    // run) and the file is waiting on the user's decision.
    let verdict = scan.is_some() && !scanning;

    // The tab body is pinned to one height for every tab, so the progress
    // bar, buttons and connections panel below never jump when switching.
    let mut col = column![
        tabs,
        container(crate::ui::scroll(tab_body))
            .width(Length::Fill)
            .height(210.0)
            .padding(12)
            .style(theme::panel),
        // The bytes are all in while a scan runs: the determinate bar has
        // nothing left to say, so it gives way to the marquee.
        if scanning {
            marquee(scan.map(ScanState::sweep).unwrap_or(0.0))
        } else {
            progress_bar(0.0..=1.0, d.disp_progress.clamp(0.0, 1.0))
                .girth(18.0)
                .style(theme::progress)
                .into()
        },
        row![
            dlg_btn(
                if p.details {
                    format!("<< {}", tr("Hide details"))
                } else {
                    format!("{} >>", tr("Show details"))
                },
                Some(Message::ProgToggleDetails(id)),
            ),
            iced::widget::space::horizontal(),
            // Scanning: Pause becomes Skip (the transfer is over, only the
            // scanner can still be stopped) and Cancel goes read-only —
            // there is nothing left to cancel, the file is on disk. Once a
            // verdict is in, the pair becomes the decision it asks for:
            // keep the file, or take it off the list and the disk. Keep is
            // the default button — deleting is the irreversible one.
            if scanning {
                dlg_btn_primary(tr("Skip"), Some(Message::ProgScanSkip(id)))
            } else if verdict {
                dlg_btn_primary(tr("Keep file"), Some(Message::ProgScanKeep(id)))
            } else {
                dlg_btn_primary(pause_label, Some(Message::ProgPauseResume(id)))
            },
            if verdict {
                dlg_btn(tr("Delete file"), Some(Message::ProgScanDelete(id)))
            } else {
                dlg_btn(tr("Cancel"), (!scanning).then_some(Message::ProgCancel(id)))
            },
        ]
        .spacing(10)
        .align_y(iced::Alignment::Center),
    ]
    .spacing(10)
    .padding(12)
    .width(Length::Fill);

    if p.details {
        // Once the scanner has the file, the details panel is its console:
        // the chunk strip and connection rows describe a finished transfer.
        if let Some(st) = scan {
            col = col.push(crate::windows::centered(
                tr("Virus scanner output"),
                theme::FONT_SIZE,
            ));
            col = col.push(scan_log(st));
        } else {
            col = col.push(crate::windows::centered(
                tr("Start positions and download progress by connections"),
                theme::FONT_SIZE,
            ));
            col = col.push(chunk_strip(d));
            col = col.push(conn_table(d));
        }
    } else {
        col = col.push(iced::widget::space::vertical());
    }
    let col = col.height(Length::Fill);

    container(col)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::window)
        .into()
}

/// Window title: `4% meilisearch-linux-aarch64` while receiving.
pub fn title(app: &App, id: crate::model::DlId) -> String {
    match app.item(id) {
        Some(d) => {
            // A recording has no total, so there is no percentage to show.
            // The elapsed media is the number someone watching it wants.
            if d.stream.as_ref().is_some_and(|s| s.live) && d.state != DlState::Complete {
                let clock = d
                    .recorded_secs
                    .map(|s| crate::fmt::eta(s as u64))
                    .unwrap_or_else(|| "0:00".into());
                return format!("{clock} {}", d.file_name);
            }
            let pct = d
                .size
                .filter(|s| *s > 0 && d.state != DlState::Complete)
                .map(|s| format!("{:.0}% ", d.downloaded as f64 * 100.0 / s as f64))
                .unwrap_or_default();
            format!("{pct}{}", d.file_name)
        }
        None => "Hydra".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ProgState;

    /// The progress dialog says why an address is unusable while the
    /// download is stopped and the box can still be corrected — and says
    /// nothing about text left in the box under another choice.
    #[test]
    fn a_bad_address_is_reported_only_while_it_is_being_asked_for() {
        let bad = ProgState {
            proxy_pick: ProxyPick::Custom,
            proxy_spec: "gopher://p".into(),
            ..ProgState::default()
        };
        assert!(proxy_problem(&bad).is_some());
        assert_eq!(
            proxy_problem(&ProgState {
                proxy_pick: ProxyPick::Direct,
                ..bad.clone()
            }),
            None
        );
        assert_eq!(
            proxy_problem(&ProgState {
                proxy_spec: "socks5://127.0.0.1:10808".into(),
                ..bad.clone()
            }),
            None
        );
        assert_eq!(
            proxy_problem(&ProgState {
                proxy_spec: String::new(),
                ..bad
            }),
            None
        );
    }

    #[test]
    fn standard_size_matches_details_mode() {
        assert_eq!(standard_size(true), (WIDTH, HEIGHT_DETAILS));
        assert_eq!(standard_size(false), (WIDTH, HEIGHT_COLLAPSED));
    }
}
