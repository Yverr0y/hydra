// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native macOS menu bar via `muda`.
//!
//! On Windows and Linux the menu bar is drawn inside the main window;
//! on macOS that would break platform convention, so the same menu model
//! (`ui::menu::entries`) is installed as an `NSMenu` and its activations come
//! back as [`crate::app::Message::NativeMenu`] ids through a channel the
//! subscription drains.

#![cfg(target_os = "macos")]

use crate::app::MenuAction;
use crate::i18n::tr;
use crate::model::{Column, SortKey};
use std::cell::RefCell;
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};

/// The toggle/selection state the menu bar must display.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub theme_mode: crate::model::ThemeMode,
    pub show_categories: bool,
    pub show_toolbar_labels: bool,
    pub ui_scale_pct: u16,
    pub language: String,
    pub speed_limiter: bool,
    /// Index of the ticked speed profile, if the cap in force matches one.
    pub speed_profile: Option<usize>,
    pub sort: (SortKey, bool),
}

/// The installed menu, plus the check items whose tick has to track the
/// settings behind them.
struct Installed {
    /// Kept alive: AppKit retains the NSMenu, muda routes callbacks through
    /// these wrappers, and a language switch rebuilds from here.
    _menu: Menu,
    hide_categories: CheckMenuItem,
    hide_toolbar_text: CheckMenuItem,
    speed_limiter: CheckMenuItem,
    speed_profiles: Vec<CheckMenuItem>,
    sort_items: Vec<(SortKey, CheckMenuItem)>,
    sort_az: CheckMenuItem,
    sort_za: CheckMenuItem,
    themes: Vec<(crate::model::ThemeMode, CheckMenuItem)>,
    scales: Vec<(u16, CheckMenuItem)>,
    languages: Vec<(String, CheckMenuItem)>,
}

thread_local! {
    /// The installed NSMenu's Rust wrappers (main thread only).
    static CURRENT: RefCell<Option<Installed>> = const { RefCell::new(None) };
}

fn item(label: &str, action: MenuAction) -> MenuItem {
    MenuItem::with_id(action.id(), tr(label), true, None)
}

/// Same, with a key equivalent shown next to the label. Used where AppKit
/// would otherwise claim the combo for itself — see the Quit item.
fn item_accel(label: &str, action: MenuAction, accel: &str) -> MenuItem {
    MenuItem::with_id(action.id(), tr(label), true, accel.parse().ok())
}

fn check(label: &str, action: MenuAction, on: bool) -> CheckMenuItem {
    CheckMenuItem::with_id(action.id(), tr(label), true, on, None)
}

/// Install the menu bar. Must run on the main thread with NSApp up (called
/// from the first `WindowOpened`); later calls are no-ops.
pub fn install(
    state: &MenuState,
    queues: &[String],
    profiles: &[crate::model::SpeedProfile],
    languages: &[String],
) {
    let installed = CURRENT.with(|c| c.borrow().is_some());
    if installed {
        return;
    }
    reinstall(state, queues, profiles, languages);
}

/// Rebuild with the current locale/state and swap it in — called after any
/// toggle the menu displays (language, dark mode, categories, font, limiter).
pub fn reinstall(
    state: &MenuState,
    queues: &[String],
    profiles: &[crate::model::SpeedProfile],
    languages: &[String],
) {
    crate::menubus::ensure_menu_handler();

    let menu = Menu::new();

    let app_m = Submenu::new("Hydra", true);
    let _ = app_m.append_items(&[
        &item("About Hydra", MenuAction::About),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(None),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        // Not `PredefinedMenuItem::quit`: that one calls AppKit's terminate
        // straight away, so the download list and config never get their
        // final flush. Our own Exit saves first, then exits.
        &item_accel("Exit Hydra", MenuAction::Exit, "Cmd+Q"),
    ]);
    let _ = menu.append(&app_m);

    let tasks = Submenu::new(tr("Tasks"), true);
    let _ = tasks.append_items(&[
        &item("Add new download", MenuAction::AddNewDownload),
        &item("Add batch download", MenuAction::AddBatch),
        &item(
            "Add batch download from clipboard",
            MenuAction::AddBatchClipboard,
        ),
        &item(
            "Add batch download from text file",
            MenuAction::AddBatchFile,
        ),
        &PredefinedMenuItem::separator(),
        &item("Export download URLs", MenuAction::ExportUrls),
    ]);
    let _ = menu.append(&tasks);

    let file = Submenu::new(tr("File"), true);
    let _ = file.append_items(&[
        &item("Stop Download", MenuAction::StopDownload),
        &item("Remove", MenuAction::Remove),
        &item("Download Now", MenuAction::DownloadNow),
        &item("Redownload", MenuAction::Redownload),
        &PredefinedMenuItem::separator(),
        &item("Export settings", MenuAction::ExportSettings),
        &item("Import settings", MenuAction::ImportSettings),
    ]);
    let _ = menu.append(&file);

    let downloads = Submenu::new(tr("Downloads"), true);
    let _ = downloads.append_items(&[
        &item("Pause All", MenuAction::PauseAll),
        &item("Stop All", MenuAction::StopAll),
        &PredefinedMenuItem::separator(),
        &item("Delete All Completed", MenuAction::DeleteAllCompleted),
        &PredefinedMenuItem::separator(),
        &item("Scheduler", MenuAction::Scheduler),
    ]);
    let start_q = Submenu::new(tr("Start queue"), true);
    let stop_q = Submenu::new(tr("Stop queue"), true);
    for q in queues {
        let _ = start_q.append(&item(q, MenuAction::StartQueue(q.clone())));
        let _ = stop_q.append(&item(q, MenuAction::StopQueue(q.clone())));
    }
    let _ = downloads.append(&start_q);
    let _ = downloads.append(&stop_q);
    let speed_limiter = check(
        "Speed Limiter",
        MenuAction::SpeedLimiterToggle,
        state.speed_limiter,
    );
    let _ = downloads.append(&speed_limiter);
    let speed_m = Submenu::new(tr("Speed limit profiles"), true);
    let mut speed_profiles = Vec::new();
    for (i, p) in profiles.iter().enumerate() {
        let label = match p.limit {
            Some(_) => format!("{} \u{2014} {}", tr(&p.name), crate::fmt::limit(p.limit)),
            None => tr(&p.name),
        };
        let it = CheckMenuItem::with_id(
            MenuAction::SpeedProfile(p.name.clone()).id(),
            label,
            true,
            state.speed_profile == Some(i),
            None,
        );
        let _ = speed_m.append(&it);
        speed_profiles.push(it);
    }
    let _ = speed_m.append(&PredefinedMenuItem::separator());
    let _ = speed_m.append(&item(
        "Speed limit settings",
        MenuAction::SpeedLimitSettings,
    ));
    let _ = downloads.append(&speed_m);
    let _ = downloads.append_items(&[
        &PredefinedMenuItem::separator(),
        &item("Options", MenuAction::Options),
    ]);
    let _ = menu.append(&downloads);

    let view = Submenu::new(tr("View"), true);
    let hide_categories = check(
        "Hide categories",
        MenuAction::HideCategories,
        !state.show_categories,
    );
    let _ = view.append(&hide_categories);
    let hide_toolbar_text = check(
        "Hide toolbar text",
        MenuAction::HideToolbarText,
        !state.show_toolbar_labels,
    );
    let _ = view.append(&hide_toolbar_text);
    let _ = view.append(&item("Columns", MenuAction::ManageColumns));
    let arrange = Submenu::new(tr("Arrange files"), true);
    let mut sort_items = Vec::new();
    // Q holds an icon, not a value a reader can arrange by.
    for col in Column::ALL.into_iter().filter(|c| *c != Column::Queue) {
        let key = SortKey::Column(col);
        let it = check(col.label(), MenuAction::ArrangeBy(key), state.sort.0 == key);
        let _ = arrange.append(&it);
        sort_items.push((key, it));
    }
    let add_it = check(
        "By order of addition",
        MenuAction::ArrangeBy(SortKey::OrderOfAddition),
        state.sort.0 == SortKey::OrderOfAddition,
    );
    let _ = arrange.append(&add_it);
    sort_items.push((SortKey::OrderOfAddition, add_it));
    let _ = arrange.append(&PredefinedMenuItem::separator());
    let sort_az = check("A-Z", MenuAction::SortDirection(true), state.sort.1);
    let _ = arrange.append(&sort_az);
    let sort_za = check("Z-A", MenuAction::SortDirection(false), !state.sort.1);
    let _ = arrange.append(&sort_za);
    let _ = view.append(&arrange);
    let theme_m = Submenu::new(tr("Theme"), true);
    let mut themes = Vec::new();
    for (label, mode) in crate::theme::THEME_CHOICES {
        let it = check(label, MenuAction::SetTheme(mode), state.theme_mode == mode);
        let _ = theme_m.append(&it);
        themes.push((mode, it));
    }
    let _ = view.append(&theme_m);
    let scale_m = Submenu::new(tr("Scale"), true);
    let mut scales = Vec::new();
    for pct in crate::theme::SCALE_STEPS {
        // A bare number: `tr` leaves it alone unless a catalogue localises
        // the digits, which is exactly what a locale that wants ۱۴۰٪ needs.
        let it = check(
            &format!("{pct}%"),
            MenuAction::UiScale(pct),
            state.ui_scale_pct == pct,
        );
        let _ = scale_m.append(&it);
        scales.push((pct, it));
    }
    let _ = view.append(&scale_m);
    let lang = Submenu::new(tr("Language"), true);
    let mut langs = Vec::new();
    for l in languages {
        let it = CheckMenuItem::with_id(
            MenuAction::Language(l.clone()).id(),
            crate::i18n::display_name(l),
            true,
            state.language == *l,
            None,
        );
        let _ = lang.append(&it);
        langs.push((l.clone(), it));
    }
    let _ = view.append(&lang);
    let _ = menu.append(&view);

    let help = Submenu::new(tr("Help"), true);
    let _ = help.append_items(&[
        &item("Hydra Home Page", MenuAction::HomePage),
        &item("Contribute on GitHub", MenuAction::Contribute),
        &item("Keyboard Shortcuts", MenuAction::Shortcuts),
        &item("Permissions", MenuAction::Permissions),
        &item("Logs", MenuAction::Logs),
        &item("Report an Issue", MenuAction::ReportIssue),
        &item("Check for updates", MenuAction::CheckUpdates),
        &item("About Hydra", MenuAction::About),
    ]);
    let _ = menu.append(&help);

    menu.init_for_nsapp();
    // Keep the Rust wrappers alive and drop the previous generation.
    CURRENT.with(|c| {
        *c.borrow_mut() = Some(Installed {
            _menu: menu,
            hide_categories,
            hide_toolbar_text,
            speed_limiter,
            speed_profiles,
            sort_items,
            sort_az,
            sort_za,
            themes,
            scales,
            languages: langs,
        });
    });
}

/// Re-tick the installed menu from `state`, in place. `false` when no menu is
/// installed yet (nothing to sync; the caller reinstalls).
///
/// AppKit flips a check item the moment it is clicked — muda does it in its
/// own item handler, before the app sees the activation — so the tick a menu
/// is left with is "whatever was clicked last", not what the setting says.
/// Left alone, View > Scale showed both the old and the new percentage
/// ticked, and clicking the one already in use unticked it. The groups here
/// are radio sets and toggles over settings the app owns, so every one of
/// them is set from the settings rather than trusted to have toggled itself.
pub fn sync(state: &MenuState) -> bool {
    CURRENT.with(|c| {
        let borrow = c.borrow();
        let Some(installed) = borrow.as_ref() else {
            return false;
        };
        installed
            .hide_categories
            .set_checked(!state.show_categories);
        installed
            .hide_toolbar_text
            .set_checked(!state.show_toolbar_labels);
        installed.speed_limiter.set_checked(state.speed_limiter);
        for (i, item) in installed.speed_profiles.iter().enumerate() {
            item.set_checked(state.speed_profile == Some(i));
        }
        for (key, item) in &installed.sort_items {
            item.set_checked(*key == state.sort.0);
        }
        installed.sort_az.set_checked(state.sort.1);
        installed.sort_za.set_checked(!state.sort.1);
        for (mode, item) in &installed.themes {
            item.set_checked(*mode == state.theme_mode);
        }
        for (pct, item) in &installed.scales {
            item.set_checked(*pct == state.ui_scale_pct);
        }
        for (lang, item) in &installed.languages {
            item.set_checked(*lang == state.language);
        }
        true
    })
}
