// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! The menu model (shared with the native macOS menu bar) and the in-window
//! menu bar + dropdowns drawn on Windows/Linux.

use crate::app::{App, El, MenuAction, MenuBarKind, Message};
use crate::model::{Column, DlState};
use crate::{i18n::tr, theme};
use iced::widget::{button, column, container, mouse_area, row, space, text};
use iced::Length;

/// Flyouts longer than this many rows scroll instead of growing past the
/// window (the Language menu lists 30+ locales).
const FLYOUT_MAX_ROWS: usize = 10;
/// Height of one dropdown row: the label's line height (iced's default is
/// 1.3 × font size) plus the button's vertical padding.
const ROW_H: f32 = theme::FONT_SIZE * 1.3 + 10.0;
/// Height of a separator: the hairline plus `sep_row`'s vertical padding.
const SEP_H: f32 = 1.0 + 6.0;
/// The narrowest a panel and a flyout are drawn, and the widest either may
/// grow to before a label is left to the panel's own clip. Between the two
/// the box is sized to what it holds; see `panel_width`.
const PANEL_W: f32 = 260.0;
const FLYOUT_W: f32 = 230.0;
const MAX_PANEL_W: f32 = 560.0;
const PANEL_PAD: f32 = 4.0;
/// A row's horizontal button padding, and the room the submenu arrow needs
/// beside the longest label.
const ROW_PAD: f32 = 28.0;
const SUB_ARROW_W: f32 = 20.0;

pub struct Entry {
    pub action: Option<MenuAction>,
    pub label: String,
    pub enabled: bool,
    /// Draw a separator above this entry.
    pub sep: bool,
    pub checked: bool,
    pub submenu: Vec<Entry>,
}

impl Entry {
    fn item(label: String, action: MenuAction) -> Self {
        Entry {
            action: Some(action),
            label,
            enabled: true,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    /// Public constructor for ad-hoc dropdowns (toolbar split buttons).
    pub fn plain(label: String, action: MenuAction, enabled: bool) -> Self {
        Entry {
            action: Some(action),
            label,
            enabled,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    fn disabled(label: String) -> Self {
        Entry {
            action: None,
            label,
            enabled: false,
            sep: false,
            checked: false,
            submenu: vec![],
        }
    }

    fn sub(label: String, submenu: Vec<Entry>) -> Self {
        Entry {
            action: None,
            label,
            enabled: true,
            sep: false,
            checked: false,
            submenu,
        }
    }

    fn sep(mut self) -> Self {
        self.sep = true;
        self
    }

    pub fn check(mut self, on: bool) -> Self {
        self.checked = on;
        self
    }
}

pub const BAR: [(MenuBarKind, &str); 5] = [
    (MenuBarKind::Tasks, "Tasks"),
    (MenuBarKind::File, "File"),
    (MenuBarKind::Downloads, "Downloads"),
    (MenuBarKind::View, "View"),
    (MenuBarKind::Help, "Help"),
];

/// The Speed Limiter's profile list: every profile with the cap it sets,
/// ticked on the one in force, and a way through to where they are edited.
///
/// Shared by the toolbar's split button and Downloads > Speed limit profiles,
/// so the quick control and the menu bar can never drift apart.
pub fn speed_entries(app: &App) -> Vec<Entry> {
    let active = app.cfg.settings.active_profile();
    let mut items: Vec<Entry> = app
        .cfg
        .settings
        .speed_profiles
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let label = match p.limit {
                Some(_) => format!("{} \u{2014} {}", tr(&p.name), crate::fmt::limit(p.limit)),
                None => tr(&p.name),
            };
            Entry::plain(label, MenuAction::SpeedProfile(p.name.clone()), true)
                .check(active == Some(i))
        })
        .collect();
    items.push(Entry::item(tr("Speed limit settings"), MenuAction::SpeedLimitSettings).sep());
    items
}

/// Entries of one top-level menu, with enabled state derived from `app`.
pub fn entries(kind: MenuBarKind, app: &App) -> Vec<Entry> {
    let sel = app.selected_item();
    let sel_active = sel.map(|d| d.state.is_active()).unwrap_or(false);
    let sel_stopped = sel
        .map(|d| matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued))
        .unwrap_or(false);
    let any_active = app.state.downloads.iter().any(|d| d.state.is_active());
    match kind {
        MenuBarKind::Tasks => vec![
            Entry::item(tr("Add new download"), MenuAction::AddNewDownload),
            Entry::item(tr("Add batch download"), MenuAction::AddBatch),
            Entry::item(
                tr("Add batch download from clipboard"),
                MenuAction::AddBatchClipboard,
            ),
            Entry::item(
                tr("Add batch download from text file"),
                MenuAction::AddBatchFile,
            ),
            Entry::disabled(tr("Run site grabber")),
            Entry::disabled(tr("Show drop target")).sep(),
            Entry::item(tr("Export download URLs"), MenuAction::ExportUrls).sep(),
            Entry::item(tr("Exit"), MenuAction::Exit).sep(),
        ],
        MenuBarKind::File => vec![
            Entry {
                enabled: sel_active,
                ..Entry::item(tr("Stop Download"), MenuAction::StopDownload)
            },
            Entry {
                enabled: sel.is_some(),
                ..Entry::item(tr("Remove"), MenuAction::Remove)
            },
            Entry {
                enabled: sel_stopped,
                ..Entry::item(tr("Download Now"), MenuAction::DownloadNow)
            },
            Entry {
                enabled: sel.is_some(),
                ..Entry::item(tr("Redownload"), MenuAction::Redownload)
            },
            Entry::item(tr("Export settings"), MenuAction::ExportSettings).sep(),
            Entry::item(tr("Import settings"), MenuAction::ImportSettings),
        ],
        MenuBarKind::Downloads => {
            let queues: Vec<Entry> = app
                .cfg
                .queues
                .iter()
                .map(|q| Entry::item(tr(&q.name), MenuAction::StartQueue(q.name.clone())))
                .collect();
            let stops: Vec<Entry> = app
                .cfg
                .queues
                .iter()
                .map(|q| Entry::item(tr(&q.name), MenuAction::StopQueue(q.name.clone())))
                .collect();
            vec![
                Entry {
                    enabled: any_active,
                    ..Entry::item(tr("Pause All"), MenuAction::PauseAll)
                },
                Entry {
                    enabled: any_active,
                    ..Entry::item(tr("Stop All"), MenuAction::StopAll)
                },
                Entry::item(tr("Delete All Completed"), MenuAction::DeleteAllCompleted).sep(),
                Entry::disabled(tr("Find")).sep(),
                Entry::item(tr("Scheduler"), MenuAction::Scheduler).sep(),
                Entry::sub(tr("Start queue"), queues),
                Entry::sub(tr("Stop queue"), stops),
                Entry::item(tr("Speed Limiter"), MenuAction::SpeedLimiterToggle)
                    .check(app.cfg.settings.speed_limiter_on)
                    .sep(),
                Entry::sub(tr("Speed limit profiles"), speed_entries(app)),
                Entry::item(tr("Options"), MenuAction::Options).sep(),
            ]
        }
        MenuBarKind::View => vec![
            Entry::item(tr("Hide categories"), MenuAction::HideCategories)
                .check(!app.cfg.settings.show_categories),
            Entry::item(tr("Hide toolbar text"), MenuAction::HideToolbarText)
                .check(!app.cfg.settings.show_toolbar_labels),
            Entry::item(tr("Columns"), MenuAction::ManageColumns),
            Entry::sub(
                tr("Arrange files"),
                // Q holds an icon, not a value a reader can arrange by.
                Column::ALL
                    .into_iter()
                    .filter(|c| *c != Column::Queue)
                    .map(|c| Entry::item(tr(c.label()), MenuAction::ArrangeBy(c)))
                    .collect(),
            ),
            Entry::sub(
                tr("Theme"),
                crate::theme::THEME_CHOICES
                    .into_iter()
                    .map(|(l, m)| {
                        Entry::item(tr(l), MenuAction::SetTheme(m))
                            .check(app.cfg.settings.theme() == m)
                    })
                    .collect(),
            )
            .sep(),
            Entry::sub(
                tr("Scale"),
                crate::theme::SCALE_STEPS
                    .into_iter()
                    .map(|pct| {
                        Entry::item(format!("{pct}%"), MenuAction::UiScale(pct))
                            .check(app.cfg.settings.ui_scale_pct == pct)
                    })
                    .collect(),
            ),
            Entry::sub(
                tr("Language"),
                crate::i18n::available()
                    .into_iter()
                    .map(|tag| {
                        let cur = app.cfg.language.clone().unwrap_or_else(|| "en".into());
                        let cur = if cur == "English" {
                            "en".to_string()
                        } else {
                            cur
                        };
                        Entry::item(
                            crate::i18n::display_name(&tag),
                            MenuAction::Language(tag.clone()),
                        )
                        .check(cur == tag)
                    })
                    .collect(),
            ),
        ],
        MenuBarKind::Help => vec![
            Entry::item(tr("Hydra Home Page"), MenuAction::HomePage),
            Entry::item(tr("Contribute on GitHub"), MenuAction::Contribute),
            Entry::item(tr("Keyboard Shortcuts"), MenuAction::Shortcuts),
            Entry::item(tr("Permissions"), MenuAction::Permissions),
            Entry::item(tr("Logs"), MenuAction::Logs),
            Entry::item(tr("Report an Issue"), MenuAction::ReportIssue),
            Entry::item(tr("Check for updates"), MenuAction::CheckUpdates).sep(),
            Entry::item(tr("About Hydra"), MenuAction::About).sep(),
        ],
    }
}

/// Context-menu entries for the table header, right-clicked on `col`: the
/// column's own two moves, then a tick per column to show or hide it, then
/// the manage dialog for the same choices in one place.
pub fn header_entries(app: &App, col: Column) -> Vec<Entry> {
    let cols = &app.cfg.settings.columns;
    let shown = |side: bool| {
        let i = cols.iter().position(|p| p.id == col);
        i.is_some_and(|i| {
            if side {
                cols[..i].iter().any(|p| p.visible)
            } else {
                cols[i + 1..].iter().any(|p| p.visible)
            }
        })
    };
    let mut v = vec![
        Entry {
            enabled: shown(true),
            ..Entry::item(tr("Move Left"), MenuAction::MoveColumn(col, true))
        },
        Entry {
            enabled: shown(false),
            ..Entry::item(tr("Move Right"), MenuAction::MoveColumn(col, false))
        },
    ];
    v.push(
        Entry::sub(
            tr("Show columns"),
            cols.iter()
                .map(|p| {
                    Entry {
                        // File Name names the row: a table without it is a
                        // grid of sizes and dates with nothing to read them
                        // against, so that one tick cannot be cleared.
                        enabled: p.id != Column::Name,
                        ..Entry::item(tr(p.id.label()), MenuAction::ToggleColumn(p.id))
                            .check(p.visible)
                    }
                })
                .collect(),
        )
        .sep(),
    );
    v.push(Entry::item(tr("Columns"), MenuAction::ManageColumns));
    v
}

/// Context-menu entries for the selected download row.
pub fn context_entries(app: &App) -> Vec<Entry> {
    let Some(d) = app.selected_item() else {
        return vec![];
    };
    let mut v = vec![];
    let done = d.state == DlState::Complete;
    if done {
        v.push(Entry::item(tr("Open"), MenuAction::OpenSel));
        v.push(Entry::item(tr("Open with..."), MenuAction::OpenWithSel));
    }
    v.push(Entry::item(tr("Open folder"), MenuAction::OpenFolderSel));
    v.push(Entry {
        enabled: !d.state.is_active(),
        ..Entry::item(tr("Move/Rename..."), MenuAction::MoveRenameSel)
    });
    v.push(Entry {
        enabled: matches!(d.state, DlState::Paused | DlState::Error | DlState::Queued),
        ..Entry::item(tr("Resume Download"), MenuAction::DownloadNow).sep()
    });
    v.push(Entry {
        enabled: d.state.is_active(),
        ..Entry::item(tr("Stop Download"), MenuAction::StopDownload)
    });
    v.push(Entry::item(tr("Redownload"), MenuAction::Redownload));
    v.push(Entry::item(tr("Delete"), MenuAction::Remove).sep());
    let queues: Vec<Entry> = app
        .cfg
        .queues
        .iter()
        .map(|q| Entry::item(tr(&q.name), MenuAction::MoveToQueue(q.name.clone())))
        .collect();
    v.push(Entry::sub(tr("Move to queue"), queues).sep());
    v.push(Entry {
        enabled: d.queue.is_some(),
        ..Entry::item(tr("Remove from queue"), MenuAction::RemoveFromQueue)
    });
    v.push(Entry::item(tr("Properties"), MenuAction::Properties).sep());
    v
}

/// The in-window menu bar (Windows/Linux; macOS uses the native bar).
pub fn bar(app: &App) -> El<'_> {
    let mut r = row![].spacing(2).padding([2, 6]);
    for (kind, label) in BAR {
        // The title of the open menu stays highlighted while its dropdown is
        // up, so hovering across the bar clearly shows which menu is showing.
        let open = app.open_menu == Some(kind);
        r = r.push(
            button(text(tr(label)).size(theme::FONT_SIZE + 1.0))
                .padding([3, 10])
                .style(move |t: &iced::Theme, s: button::Status| {
                    theme::btn_menu(t, if open { button::Status::Hovered } else { s })
                })
                .on_press(Message::MenuOpen(kind)),
        );
    }
    r.into()
}

/// Invisible clone of a menu-bar button: identical text/padding, so it
/// occupies exactly the same space as the real one without being seen.
fn ghost<'a>(label: &str) -> iced::widget::Button<'a, Message> {
    button(
        text(tr(label))
            .size(theme::FONT_SIZE + 1.0)
            .color(iced::Color::TRANSPARENT),
    )
    .padding([3, 10])
    .style(|_t: &iced::Theme, _s: button::Status| button::Style::default())
}

/// Full-window overlay for a menu-bar dropdown. Instead of estimating pixel
/// offsets, the bar's layout is replayed with invisible button clones and the
/// panel slots in right after them — so it lands exactly under its button for
/// any font, size, or locale.
pub fn bar_overlay(app: &App, kind: MenuBarKind) -> El<'_> {
    let items = entries(kind, app);
    let panel = dropdown(&items, app.open_submenu);
    // Same height as the real bar row (+1 for the rule below it). Every title
    // is replayed here, not just the first: the overlay covers the real bar,
    // so these ghosts are what the pointer actually reaches while a menu is
    // open. Hovering one switches the dropdown to it (menu tracking, like IDM
    // and every native Windows/Linux menu bar); pressing the open title again
    // closes the bar.
    let mut bar_ghost = row![].spacing(2).padding([2, 6]);
    for (k, label) in BAR {
        bar_ghost = bar_ghost.push(
            mouse_area(ghost(label))
                .on_enter(Message::MenuHover(k))
                .on_press(Message::MenuOpen(k)),
        );
    }
    // The buttons left of the open one push the panel to its x position.
    let mut below = row![].spacing(2).padding(iced::Padding {
        left: 6.0,
        ..iced::Padding::ZERO
    });
    for (k, label) in BAR {
        if k == kind {
            break;
        }
        below = below.push(ghost(label));
    }
    below = below.push(panel);
    let content = column![bar_ghost, space::vertical().height(1.0), below];
    iced::widget::stack![
        mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill))
            .on_press(Message::MenuClose)
            .on_right_press(Message::MenuClose),
        content,
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// The separator wrapper above an entry: only the inner 1px line is painted;
/// the padded wrapper stays transparent (styling the wrapper fills its
/// padding too and renders a thick gray band instead of a hairline). The
/// `visible: false` variant is a ghost spacer for flyout alignment.
fn sep_row<'a>(visible: bool) -> El<'a> {
    let line = container(space::horizontal().height(1.0)).width(Length::Fill);
    let line = if visible {
        line.style(|t: &iced::Theme| container::Style {
            background: Some(iced::Background::Color(theme::grid_line(t))),
            ..Default::default()
        })
    } else {
        line
    };
    container(line).width(Length::Fill).padding([3, 6]).into()
}

/// What a row draws: the tick column, then the label. The column is
/// spaces rather than an offset so an unticked row lines up with a ticked
/// one, and so one measurement covers both.
fn row_label(e: &Entry) -> String {
    let check = if e.checked { "✓  " } else { "    " };
    format!("{check}{}", e.label)
}

/// The width a panel needs to hold `items`, never below `min`.
///
/// Measured rather than fixed: the same menu is 33 characters in English
/// and 64 in Portuguese, and iced paints a label that does not fit over
/// whatever sits beside it instead of clipping it. Rows stay one line tall
/// at this width, which is what `panel_height` and the flyout's ghost
/// alignment are built on.
fn panel_width(items: &[Entry], min: f32) -> f32 {
    items
        .iter()
        .map(|e| {
            let arrow = if e.submenu.is_empty() {
                0.0
            } else {
                SUB_ARROW_W
            };
            crate::font::line_width(&row_label(e), theme::FONT_SIZE) + arrow + ROW_PAD
        })
        .fold(min, f32::max)
        .min(MAX_PANEL_W)
}

/// One dropdown entry row (label + optional check mark / submenu arrow).
fn entry_row<'a>(e: &Entry, i: usize, open_submenu: Option<usize>) -> El<'a> {
    let has_sub = !e.submenu.is_empty();
    let is_open = has_sub && open_submenu == Some(i);
    let label_row = row![
        text(row_label(e))
            .size(theme::FONT_SIZE)
            .wrapping(text::Wrapping::None),
        space::horizontal(),
        text(if has_sub { "›" } else { "" }).size(theme::FONT_SIZE),
    ]
    .width(Length::Fill);
    let msg = if has_sub {
        Some(Message::SubmenuHover(Some(i)))
    } else {
        e.action.clone().filter(|_| e.enabled).map(Message::Menu)
    };
    let enabled = e.enabled;
    let mut b = button(label_row)
        .padding([5, 14])
        .width(Length::Fill)
        .style(move |t: &iced::Theme, s: button::Status| {
            if !enabled {
                return button::Style {
                    background: None,
                    text_color: theme::dim_text(t),
                    ..button::Style::default()
                };
            }
            // The parent of an open flyout stays highlighted like macOS.
            theme::btn_menu(t, if is_open { button::Status::Hovered } else { s })
        });
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    // Hovering opens an entry's flyout and closes any sibling's — the macOS
    // behaviour; entries without children close the open flyout instead.
    mouse_area(b)
        .on_enter(Message::SubmenuHover(has_sub.then_some(i)))
        .into()
}

/// Invisible clone of an entry row: identical text/padding so it occupies the
/// exact height of the real row, used to align the flyout with its parent.
fn ghost_entry<'a>(e: &Entry) -> El<'a> {
    button(
        text(row_label(e))
            .size(theme::FONT_SIZE)
            .wrapping(text::Wrapping::None)
            .color(iced::Color::TRANSPARENT),
    )
    .padding([5, 14])
    .width(Length::Fill)
    .style(|_t: &iced::Theme, _s: button::Status| button::Style::default())
    .into()
}

/// A dropdown panel for `entries`, used by both the menu bar and the context
/// menu. An entry with children opens a macOS-style flyout box to the right
/// of the panel, top-aligned with its parent row (`open_submenu`).
pub fn dropdown<'a>(items: &[Entry], open_submenu: Option<usize>) -> El<'a> {
    let mut col = column![].spacing(0).width(Length::Shrink);
    for (i, e) in items.iter().enumerate() {
        if e.sep {
            col = col.push(sep_row(true));
        }
        col = col.push(entry_row(e, i, open_submenu));
    }
    let panel = container(col)
        .padding(PANEL_PAD)
        .width(panel_width(items, PANEL_W))
        .clip(true)
        .style(theme::menu_panel);

    let open = open_submenu
        .and_then(|i| items.get(i).map(|e| (i, e)))
        .filter(|(_, e)| !e.submenu.is_empty());
    let Some((idx, parent)) = open else {
        return panel.into();
    };

    // The flyout column replays the panel's top padding and every row above
    // the parent as invisible ghosts, so the flyout's top edge lands exactly
    // on the parent row for any font, size, or locale.
    let mut spacer = column![space::vertical().height(PANEL_PAD)].spacing(0);
    for e in items.iter().take(idx) {
        if e.sep {
            spacer = spacer.push(sep_row(false));
        }
        spacer = spacer.push(ghost_entry(e));
    }
    if parent.sep {
        spacer = spacer.push(sep_row(false));
    }

    let mut sub = column![].spacing(0).width(Length::Shrink);
    for c in &parent.submenu {
        let mut cb = button(
            text(row_label(c))
                .size(theme::FONT_SIZE)
                .wrapping(text::Wrapping::None),
        )
        .padding([5, 14])
        .width(Length::Fill)
        .style(theme::btn_menu);
        if let Some(a) = c.action.clone().filter(|_| c.enabled) {
            cb = cb.on_press(Message::Menu(a));
        }
        sub = sub.push(cb);
    }
    let sub: El<'a> = if parent.submenu.len() > FLYOUT_MAX_ROWS {
        crate::ui::scroll(sub)
            .width(Length::Fill)
            .height(ROW_H * FLYOUT_MAX_ROWS as f32)
            .into()
    } else {
        sub.into()
    };
    let flyout = container(sub)
        .padding(4)
        .width(panel_width(&parent.submenu, FLYOUT_W))
        .clip(true)
        .style(theme::menu_panel);

    row![panel, column![spacer, flyout]].into()
}

/// The height `dropdown` lays a panel out at: one row per entry, a hairline
/// above each entry that carries one, and the panel's padding. Every row is
/// a single line of text in every locale, so this is the laid-out height and
/// not a guess at it.
fn panel_height(items: &[Entry]) -> f32 {
    let seps = items.iter().filter(|e| e.sep).count() as f32;
    PANEL_PAD * 2.0 + items.len() as f32 * ROW_H + seps * SEP_H
}

/// Where a menu of `panel` opened at `at` has to sit to stay inside a window
/// of `view`.
///
/// One that does not fit below the pointer opens *above* it instead — what
/// every native menu does, and the only placement that shows the whole menu
/// for a row near the bottom edge. Taller than the window either way, it is
/// pushed against the bottom edge, which keeps the entries by the pointer
/// reachable. Horizontally the panel is a fixed width, so a menu opened near
/// the right edge only has to slide left far enough to fit.
fn anchor(at: iced::Point, panel: iced::Size, view: iced::Size) -> iced::Point {
    let x = at.x.min(view.width - panel.width).max(0.0);
    let y = if at.y + panel.height <= view.height {
        at.y
    } else if panel.height <= at.y {
        at.y - panel.height
    } else {
        (view.height - panel.height).max(0.0)
    };
    iced::Point::new(x, y)
}

/// Full-window overlay: click-away layer + positioned dropdown.
pub fn overlay<'a>(app: &'a App, items: Vec<Entry>, at: iced::Point) -> El<'a> {
    let size = iced::Size::new(panel_width(&items, PANEL_W), panel_height(&items));
    let at = anchor(at, size, app.main_viewport());
    let panel = dropdown(&items, app.open_submenu);
    iced::widget::stack![
        mouse_area(space::horizontal().width(Length::Fill).height(Length::Fill))
            .on_press(Message::MenuClose)
            .on_right_press(Message::MenuClose),
        iced::widget::pin(panel).x(at.x).y(at.y),
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Point, Size};

    fn entries(n: usize, seps: usize) -> Vec<Entry> {
        (0..n)
            .map(|i| {
                let e = Entry::plain(format!("item {i}"), MenuAction::Options, true);
                if i < seps {
                    e.sep()
                } else {
                    e
                }
            })
            .collect()
    }

    #[test]
    fn a_short_menu_keeps_the_classic_panel_width() {
        assert_eq!(panel_width(&entries(4, 0), PANEL_W), PANEL_W);
    }

    #[test]
    fn a_panel_grows_to_hold_the_longest_label_it_has() {
        // The label the German Tasks menu opens with, and the Portuguese
        // one that is nearly twice the English string.
        let long = [
            "Batch-Download aus der Zwischenablage hinzufügen",
            "Adicionar transferência em lote a partir de um ficheiro de texto",
        ];
        let mut items = entries(3, 0);
        for (i, label) in long.iter().enumerate() {
            items.push(Entry::plain((*label).into(), MenuAction::Options, true));
            let w = panel_width(&items, PANEL_W);
            assert!(w > PANEL_W, "{label} left the panel at {w}");
            assert!(
                w >= crate::font::line_width(&row_label(&items[3 + i]), theme::FONT_SIZE) + ROW_PAD,
                "{label} does not fit the {w}px panel it produced"
            );
        }
    }

    #[test]
    fn a_row_with_a_flyout_reserves_the_arrow_beside_its_label() {
        let label = "Einstellungen zur Geschwindigkeitsbegrenzung";
        let plain = vec![Entry::plain(label.into(), MenuAction::Options, true)];
        let parent = vec![Entry::sub(label.into(), entries(2, 0))];
        assert_eq!(
            panel_width(&parent, PANEL_W) - panel_width(&plain, PANEL_W),
            SUB_ARROW_W
        );
    }

    #[test]
    fn a_ticked_row_is_measured_with_its_tick_column() {
        let item = Entry::plain("Unlimited".into(), MenuAction::Options, true);
        let ticked = Entry::plain("Unlimited".into(), MenuAction::Options, true).check(true);
        assert!(row_label(&ticked).starts_with('\u{2713}'));
        // Both columns are four characters wide, so a tick never moves the
        // label or the panel edge.
        assert_eq!(
            panel_width(&[item], PANEL_W),
            panel_width(&[ticked], PANEL_W)
        );
    }

    #[test]
    fn a_label_past_all_reason_stops_at_the_panels_own_limit() {
        let items = vec![Entry::plain("x".repeat(500), MenuAction::Options, true)];
        assert_eq!(panel_width(&items, PANEL_W), MAX_PANEL_W);
    }

    #[test]
    fn panel_height_counts_rows_separators_and_padding() {
        assert_eq!(
            panel_height(&entries(4, 2)),
            PANEL_PAD * 2.0 + 4.0 * ROW_H + 2.0 * SEP_H
        );
    }

    #[test]
    fn menu_that_fits_opens_at_the_pointer() {
        let panel = Size::new(PANEL_W, 200.0);
        let view = Size::new(900.0, 600.0);
        assert_eq!(
            anchor(Point::new(120.0, 80.0), panel, view),
            Point::new(120.0, 80.0)
        );
    }

    #[test]
    fn menu_near_the_bottom_opens_above_the_pointer() {
        let panel = Size::new(PANEL_W, 200.0);
        let view = Size::new(900.0, 600.0);
        // 560 + 200 runs 160px past the window; the whole menu still fits
        // above the pointer, so that is where it goes.
        assert_eq!(
            anchor(Point::new(10.0, 560.0), panel, view),
            Point::new(10.0, 360.0)
        );
    }

    #[test]
    fn menu_exactly_reaching_the_bottom_edge_stays_below() {
        let panel = Size::new(PANEL_W, 200.0);
        let view = Size::new(900.0, 600.0);
        assert_eq!(
            anchor(Point::new(0.0, 400.0), panel, view),
            Point::new(0.0, 400.0)
        );
    }

    #[test]
    fn menu_too_tall_for_either_side_sits_on_the_bottom_edge() {
        let panel = Size::new(PANEL_W, 500.0);
        let view = Size::new(900.0, 600.0);
        assert_eq!(
            anchor(Point::new(0.0, 300.0), panel, view),
            Point::new(0.0, 100.0)
        );
    }

    #[test]
    fn menu_taller_than_the_window_starts_at_the_top() {
        let panel = Size::new(PANEL_W, 700.0);
        let view = Size::new(900.0, 600.0);
        assert_eq!(anchor(Point::new(0.0, 300.0), panel, view).y, 0.0);
    }

    #[test]
    fn menu_near_the_right_edge_slides_left_to_fit() {
        let panel = Size::new(PANEL_W, 100.0);
        let view = Size::new(900.0, 600.0);
        assert_eq!(
            anchor(Point::new(800.0, 10.0), panel, view).x,
            900.0 - PANEL_W
        );
    }

    #[test]
    fn menu_wider_than_the_window_starts_at_the_left_edge() {
        let panel = Size::new(PANEL_W, 100.0);
        let view = Size::new(200.0, 600.0);
        assert_eq!(anchor(Point::new(150.0, 10.0), panel, view).x, 0.0);
    }
}
