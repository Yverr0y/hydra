// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Help > Keyboard Shortcuts: every action with its editable combo.
//! `cmd` means ⌘ on macOS and Ctrl on Windows/Linux; edits persist to the
//! configuration immediately.

use crate::app::{App, El, Message, WinKind};
use crate::model::{normalize_combo, SHORTCUT_ACTIONS};
use crate::windows::dlg_btn_primary;
use crate::{i18n::tr, theme};
use iced::widget::{column, container, row, text, text_input};
use iced::Length;

pub fn view(app: &App) -> El<'_> {
    let mut list = column![].spacing(8);
    list = list.push(
        text(tr("cmd means Command on macOS and Ctrl on Windows/Linux. Click a field and type a combo like cmd+shift+v."))
            .size(theme::FONT_SIZE - 1.0)
            .color(theme::dim_text(&iced::Theme::Light)),
    );
    // What each row's combo normalizes to, so a conflict can be shown: two
    // actions on one combo means the later one silently never fires.
    let combos: Vec<Option<String>> = SHORTCUT_ACTIONS
        .iter()
        .map(|(id, _, _)| app.cfg.shortcuts.get(*id).and_then(|c| normalize_combo(c)))
        .collect();
    for (n, (id, default, label)) in SHORTCUT_ACTIONS.iter().enumerate() {
        let value = app
            .cfg
            .shortcuts
            .get(*id)
            .cloned()
            .unwrap_or_else(|| crate::model::platform_default(default));
        let action = id.to_string();
        let conflict = combos[n].is_some()
            && combos
                .iter()
                .enumerate()
                .any(|(m, c)| m != n && *c == combos[n]);
        let usable = combos[n].is_some() && !conflict;
        list = list.push(
            row![
                text(tr(label)).size(theme::FONT_SIZE).width(Length::Fill),
                text_input("", &value)
                    .on_input(move |v| Message::ShortcutEdit(action.clone(), v))
                    .size(theme::FONT_SIZE)
                    .style(if usable {
                        theme::input
                    } else {
                        theme::input_invalid
                    })
                    .width(150.0),
            ]
            .spacing(10)
            .align_y(iced::Alignment::Center),
        );
    }
    let problems = combos.iter().filter(|c| c.is_none()).count()
        + combos
            .iter()
            .enumerate()
            .filter(|(n, c)| c.is_some() && combos.iter().take(*n).any(|d| d == *c))
            .count();
    if problems > 0 {
        list = list.push(
            text(tr("A red combo is not one a key press can produce, or is taken by another action; it goes back to its default when this window closes."))
                .size(theme::FONT_SIZE - 1.0)
                .color(theme::error_text()),
        );
    }
    container(
        column![
            // The table grows with every new action, and View > Scale scales
            // every row: scroll rather than push OK off the bottom.
            crate::ui::scroll(list).height(Length::Fill),
            row![
                iced::widget::space::horizontal(),
                dlg_btn_primary(
                    tr("OK"),
                    app.win_of(WinKind::Shortcuts).map(Message::CloseThis)
                ),
            ],
        ]
        .spacing(10)
        .padding(16),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .style(theme::window)
    .into()
}
