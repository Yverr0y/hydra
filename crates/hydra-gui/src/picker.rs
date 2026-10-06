// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Native file and folder panels that leave the event loop running.
//!
//! `rfd`'s blocking API shows the panel on the calling thread and runs the
//! platform's own modal loop there. Called from `update`, that loop owns the
//! thread every window message arrives on for as long as the user browses:
//! the app stops repainting, and on Windows the panel then has to share its
//! message pump with the redraw traffic of every running transfer — which is
//! what made browsing for a save folder crawl while a download was live.
//!
//! The async API hands the panel to the platform instead: a thread of its own
//! on Windows, a sheet on macOS, the portal on Linux. It has one rule — the
//! panel must be *built* on the main thread, because AppKit begins the sheet
//! in the constructor rather than at first poll, which is why handing the
//! whole call to an executor thread deadlocks. So every panel here is built
//! inside `window::run`, which runs its closure on the event loop, and only
//! the future it returns is awaited off it.
//!
//! Building it there also gives the panel its owner window, which is what
//! keeps it in front of the window that asked for it now that that window is
//! no longer frozen behind it.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use iced::window;
use iced::Task;

/// What to ask for. Every field is optional because each platform panel has
/// its own default: the folder last used, an empty name box, the panel's
/// stock title.
#[derive(Clone, Default)]
pub struct Ask {
    pub title: Option<String>,
    pub directory: Option<String>,
    pub file_name: Option<String>,
    /// A display name and the extensions it covers, e.g. `("Audio", &["wav",
    /// "ogg"])`. Without one the panel offers every file.
    pub owned_filter: Option<(String, Vec<String>)>,
    pub filter: Option<(&'static str, &'static [&'static str])>,
}

impl Ask {
    /// Start in `dir`, unless it is the empty string a download carries
    /// before it has been given a destination.
    pub fn in_dir(dir: &str) -> Self {
        Self {
            directory: (!dir.is_empty()).then(|| dir.to_owned()),
            ..Self::default()
        }
    }
}

/// Ask for an existing folder.
pub fn folder(owner: Option<window::Id>, ask: Ask) -> Task<Option<PathBuf>> {
    show(owner, ask, |d| Box::pin(d.pick_folder()))
}

/// Ask for an existing file.
pub fn file(owner: Option<window::Id>, ask: Ask) -> Task<Option<PathBuf>> {
    show(owner, ask, |d| Box::pin(d.pick_file()))
}

/// Ask where to write. The panel warns about overwriting on its own.
pub fn save(owner: Option<window::Id>, ask: Ask) -> Task<Option<PathBuf>> {
    show(owner, ask, |d| Box::pin(d.save_file()))
}

type Panel = Pin<Box<dyn Future<Output = Option<rfd::FileHandle>> + Send>>;

/// `open` is a plain `fn` rather than a closure so that the task below can
/// hold it in the `Fn` that `and_then` wants.
fn show(
    owner: Option<window::Id>,
    ask: Ask,
    open: fn(rfd::AsyncFileDialog) -> Panel,
) -> Task<Option<PathBuf>> {
    // `None` is a panel with no window behind it — a permission prompt from
    // a transfer running with the app in the tray. It still has to appear,
    // and under whatever window this app does have.
    let owner = match owner {
        Some(id) => Task::done(Some(id)),
        None => window::latest(),
    };
    owner
        .and_then(move |id| {
            let ask = ask.clone();
            window::run(id, move |w| open(ask.build().set_parent(w))).then(Task::future)
        })
        .map(|picked| picked.map(|f| f.path().to_path_buf()))
}

impl Ask {
    fn build(self) -> rfd::AsyncFileDialog {
        let mut dlg = rfd::AsyncFileDialog::new();
        if let Some(title) = self.title {
            dlg = dlg.set_title(title);
        }
        if let Some(dir) = self.directory {
            dlg = dlg.set_directory(dir);
        }
        if let Some(name) = self.file_name {
            dlg = dlg.set_file_name(name);
        }
        if let Some((name, extensions)) = self.owned_filter {
            dlg = dlg.add_filter(name, &extensions);
        }
        if let Some((name, extensions)) = self.filter {
            dlg = dlg.add_filter(name, extensions);
        }
        dlg
    }
}

/// A picked path as the `String` the download list and the settings file
/// keep. Lossy: a path that is not valid Unicode is not one this app can
/// round-trip through its JSON state anyway.
pub fn into_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::Ask;

    /// A download carries an empty `save_dir` until it has been given a
    /// destination, and an empty string is not a folder: handed to the
    /// platform it is a path that resolves to nothing, and the panel opens
    /// wherever it likes instead of where the caller asked.
    #[test]
    fn a_download_with_no_destination_yet_asks_for_no_start_folder() {
        assert_eq!(Ask::in_dir("").directory, None);
        assert_eq!(
            Ask::in_dir("/Users/someone/Downloads").directory.as_deref(),
            Some("/Users/someone/Downloads")
        );
    }

    /// The panel is configured once, on the event loop, and nothing reads it
    /// back afterwards — so what an [`Ask`] fails to pass on is lost in
    /// silence rather than reported.
    #[test]
    fn what_the_caller_asked_for_is_what_the_panel_is_built_with() {
        let built = format!(
            "{:?}",
            Ask {
                title: Some("Open with...".into()),
                file_name: Some("x.zip".into()),
                filter: Some(("Audio", &["wav", "ogg"])),
                ..Ask::in_dir("/tmp/start")
            }
            .build()
        );
        for asked in ["Open with...", "x.zip", "Audio", "wav", "ogg", "/tmp/start"] {
            assert!(built.contains(asked), "the panel was never told {asked:?}");
        }
        // And the other way: an empty title bar or a filter matching nothing
        // is a panel narrower than the caller asked for.
        assert_eq!(
            format!("{:?}", Ask::default().build()),
            format!("{:?}", rfd::AsyncFileDialog::new())
        );
    }
}
