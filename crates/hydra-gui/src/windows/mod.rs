// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

pub mod about;
pub mod add_url;
pub mod batch;
pub mod columns;
pub mod complete;
pub mod confirm;
pub mod file_info;
pub mod main_win;
pub mod options;
pub mod permissions;
pub mod power;
pub mod progress;
pub mod scheduler;
pub mod shortcuts;
pub mod update;
pub mod zip_preview;

use crate::app::{El, Message};
use crate::theme;
use iced::widget::{button, text};

/// Wrap a link-ish element with a hover hint describing the file type its
/// URL points at (`DMG — Apple Disk Image (macOS)`). URLs without a known
/// extension come back unwrapped — no tooltip beats a useless one.
pub fn ext_hint<'a>(el: impl Into<El<'a>>, url: &str) -> El<'a> {
    match crate::ext_info::hint_for_url(url) {
        Some(hint) => iced::widget::tooltip(
            el,
            iced::widget::container(text(hint).size(theme::FONT_SIZE - 1.0))
                .padding(8)
                .max_width(360.0)
                .style(theme::menu_panel),
            iced::widget::tooltip::Position::Bottom,
        )
        .into(),
        None => el.into(),
    }
}

/// A settings checkbox: one size and style for every dialog, with a label
/// that may break inside a word.
///
/// The break is the point. iced draws a label that outgrows its column
/// straight over the control beside it instead of clipping it, and a
/// translated label is routinely one unbreakable compound word wider than
/// the column it was measured for in English
/// ("Warteschlangenverarbeitung wurde gestartet").
pub fn check<'a>(on: bool, label: String) -> iced::widget::Checkbox<'a, Message> {
    iced::widget::checkbox(on)
        .label(label)
        .size(15.0)
        .text_size(theme::FONT_SIZE)
        .text_wrapping(iced::widget::text::Wrapping::WordOrGlyph)
        .style(theme::check)
}

/// How wide a label has to be drawn, never below `min`.
///
/// Every fixed width in these dialogs was picked against the English
/// string, and iced paints a label that outgrows its box over whatever is
/// beside it rather than clipping it. Measuring turns each of those into a
/// floor that the longer translations lift.
pub fn label_width(label: &str, min: f32) -> f32 {
    crate::font::line_width(label, theme::FONT_SIZE).max(min)
}

/// How wide a column of `labels` has to be, never below `min`.
fn column_width<'a>(labels: impl Iterator<Item = &'a str>, min: f32) -> f32 {
    labels.fold(min, |w, label| label_width(label, w))
}

/// Rows laid out over one label column: as wide as the widest label this
/// locale produced, never narrower than `min`, with `gap` between the
/// label and its content and between the rows. A row with no label starts
/// at the content column.
///
/// Measured rather than fixed. A label column picked against English is
/// half again too narrow in German or Swedish, and iced draws the overflow
/// straight over the field beside it instead of clipping it.
pub fn label_column<'a>(rows: Vec<(Option<String>, El<'a>)>, min: f32, gap: f32) -> El<'a> {
    let w = column_width(rows.iter().filter_map(|(label, _)| label.as_deref()), min);
    let mut col = iced::widget::column![]
        .spacing(gap)
        .width(iced::Length::Fill);
    for (label, content) in rows {
        let head: El<'a> = match label {
            Some(label) => text(label)
                .size(theme::FONT_SIZE)
                .wrapping(iced::widget::text::Wrapping::None)
                .width(w)
                .into(),
            None => iced::widget::space::horizontal().width(w).into(),
        };
        col = col.push(
            iced::widget::row![head, content]
                .spacing(gap)
                .align_y(iced::Alignment::Center),
        );
    }
    col.into()
}

/// One cell of a fixed-column list: a single line, cut off at the column
/// edge.
///
/// A list column cannot grow without breaking the grid, and what goes in
/// one is a file name, a server or a translated header — all of which run
/// past it sooner or later. Cut is the only option left that does not paint
/// the cell over its neighbour.
pub fn cell<'a>(s: String, w: impl Into<iced::Length>) -> iced::widget::Container<'a, Message> {
    iced::widget::container(
        text(s)
            .size(theme::FONT_SIZE)
            .wrapping(iced::widget::text::Wrapping::None),
    )
    .width(w)
    .clip(true)
}

/// Spacer-centred text for any Fill-width context: `Text::center()`
/// mis-places some RTL runs, spacers never do. The label breaks inside a
/// word before it will run over whatever shares the row — a tab bar splits
/// its width evenly whatever the locale made the labels.
pub fn centered<'a>(label: String, size: f32) -> El<'a> {
    iced::widget::row![
        iced::widget::space::horizontal(),
        text(label)
            .size(size)
            .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
        iced::widget::space::horizontal(),
    ]
    .width(iced::Length::Fill)
    .into()
}

/// The uniform width of a dialog push button, and the padding either side
/// of its label.
const BTN_W: f32 = 132.0;
const BTN_PAD_X: f32 = 8.0;

/// How wide a dialog push button is drawn for `label`: [`BTN_W`] is the
/// floor, not the width. A row of buttons keeps its classic even look in
/// English, and a label that outgrows it — "Uncheck Selected" is 42
/// characters in Hungarian — widens its own button instead of being painted
/// over the one beside it, which is what iced does with text that does not
/// fit.
fn btn_width(label: &str) -> f32 {
    label_width(label, BTN_W - 2.0 * BTN_PAD_X - 4.0) + 2.0 * BTN_PAD_X + 4.0
}

/// A dialog button sized to its label — for rows whose labels vary too much
/// for a uniform width ("Download as new file" vs "Cancel").
pub fn dlg_btn_auto<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(text(label).size(theme::FONT_SIZE))
        .padding([5, 16])
        .style(theme::btn);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// As [`dlg_btn_auto`] with the accent border.
pub fn dlg_btn_auto_primary<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let mut b = button(text(label).size(theme::FONT_SIZE))
        .padding([5, 16])
        .style(theme::btn_primary);
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

/// A classic dialog push button: the uniform dialog width, and wider only
/// where the label needs it.
pub fn dlg_btn<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let width = btn_width(&label);
    dlg_btn_sized(label, msg, width, false)
}

/// The dialog's default button (accent border).
pub fn dlg_btn_primary<'a>(label: String, msg: Option<Message>) -> El<'a> {
    let width = btn_width(&label);
    dlg_btn_sized(label, msg, width, true)
}

fn dlg_btn_sized<'a>(label: String, msg: Option<Message>, width: f32, primary: bool) -> El<'a> {
    let mut b = button(centered(label, theme::FONT_SIZE))
        .padding([5.0, BTN_PAD_X])
        .width(width)
        .style(if primary {
            theme::btn_primary
        } else {
            theme::btn
        });
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_column_is_a_floor_the_longer_translations_lift() {
        // Short enough for the column it was drawn for: the column does not
        // move, so a dialog looks the same in English as it always has.
        assert_eq!(label_width("Size:", 84.0), 84.0);

        // Longer than it: the column takes the label, since the alternative
        // is the label taking the field beside it.
        let long = "Speichern unter:";
        let w = label_width(long, 84.0);
        assert!(w > 84.0, "{long} was squeezed into {w}px");
        assert_eq!(w, crate::font::line_width(long, theme::FONT_SIZE));
    }

    #[test]
    fn a_push_button_keeps_its_uniform_width_until_the_label_needs_more() {
        for label in ["OK", "Cancel", "Browse"] {
            assert_eq!(btn_width(label), BTN_W, "{label} resized the button row");
        }
        // 42 characters, and every one of them drawn over the next button
        // before the width was measured.
        let hungarian = "Kiválasztottak kijelölésének megszüntetése";
        let w = btn_width(hungarian);
        assert!(w > BTN_W, "{hungarian} still has to fit in {w}px");
        assert!(
            w >= crate::font::line_width(hungarian, theme::FONT_SIZE) + 2.0 * BTN_PAD_X,
            "the label touches the button's edge at {w}px"
        );
    }

    #[test]
    fn a_column_is_as_wide_as_the_widest_label_in_it() {
        // Not the first label, and not the floor: the one that needs the
        // room. Every other row lines up with it.
        let longest = "Möjlighet till återupptagning:";
        let labels = ["Status:", longest, "Size:"];
        assert_eq!(
            column_width(labels.into_iter(), 84.0),
            label_width(longest, 84.0)
        );
        // A column of short labels stays where the dialog drew it.
        assert_eq!(column_width(["Size:", "Status:"].into_iter(), 84.0), 84.0);
        // And an empty one is the floor, not zero.
        assert_eq!(column_width(std::iter::empty(), 84.0), 84.0);
    }
}
