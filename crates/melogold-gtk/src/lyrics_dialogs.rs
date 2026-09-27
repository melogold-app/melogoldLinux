//! «Найти другой текст…» (Windows `LyricsSearchDialog`, Android `LrcLibSearchDialog`): поиск по
//! LrcLib — поле сверху, под ним треки с длительностью и видом текста; выбранный текст — свой
//! (задание 0002). «Импорт из файла» — TTML, LRC или простой текст.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::text::format_duration;
use melogold_core::title_cleaner;
use melogold_innertube::lyrics::LrcLibTrack;

use crate::localization::tr;
use crate::window::MainWindow;

impl MainWindow {
    /// Запрос по умолчанию — свои название и исполнитель (задание 0005), очищенные, как у поиска текста.
    fn lyrics_query(&self) -> String {
        let Some(track) = self.lyrics.track() else { return String::new() };
        let shown = self.display(&track);
        let artist = shown.artists_text.clone().unwrap_or_default();
        let clean = title_cleaner::clean(&shown.title, (!artist.is_empty()).then_some(artist.as_str()), shown.video_type.as_deref());
        let artist = clean.artist.unwrap_or(artist);
        let title = if clean.title.trim().is_empty() { shown.title } else { clean.title };
        format!("{artist} {title}").trim().to_owned()
    }

    pub fn find_lyrics(&self) {
        if self.lyrics.track().is_none() {
            return;
        }
        let entry = gtk::SearchEntry::builder().text(self.lyrics_query()).placeholder_text(tr("LyricsFind")).hexpand(true).build();
        entry.update_property(&[gtk::accessible::Property::Label(tr("LyricsFind"))]);
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).valign(gtk::Align::Start).build();
        list.add_css_class("boxed-list");
        let spinner =
            adw::Spinner::builder().width_request(32).height_request(32).halign(gtk::Align::Center).valign(gtk::Align::Center).build();
        let empty = adw::StatusPage::builder().title(tr("NoLyricsFound")).icon_name("edit-find-symbolic").build();
        empty.add_css_class("compact");
        let results = gtk::Stack::builder().vexpand(true).build();
        let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never).child(&list).vexpand(true).build();
        results.add_named(&scroller, Some("list"));
        results.add_named(&spinner, Some("loading"));
        results.add_named(&empty, Some("empty"));
        let content = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).margin_start(12).margin_end(12).build();
        content.append(&entry);
        content.append(&results);
        // Файл — внизу, под списком: заголовок остаётся заголовком (HIG).
        let import = gtk::Button::builder().label(tr("LyricsImportFile")).halign(gtk::Align::Center).build();
        import.add_css_class("pill");
        let bottom = gtk::Box::builder().margin_top(6).margin_bottom(12).halign(gtk::Align::Center).build();
        bottom.append(&import);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.add_bottom_bar(&bottom);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder().title(tr("ChooseLyricTrack")).content_width(520).content_height(560).child(&toolbar).build();

        let token = Rc::new(Cell::new(0u64));
        let search = {
            let (weak, list, results, token) = (self.downgrade(), list.clone(), results.clone(), Rc::clone(&token));
            let dialog = dialog.downgrade();
            Rc::new(move |text: String, delay: Duration| {
                let ticket = token.get() + 1;
                token.set(ticket);
                results.set_visible_child_name("loading");
                let (weak, list, results, token, dialog) = (weak.clone(), list.clone(), results.clone(), Rc::clone(&token), dialog.clone());
                glib::spawn_future_local(async move {
                    // Ждём, пока человек допечатает.
                    glib::timeout_future(delay).await;
                    let Some(window) = weak.upgrade() else { return };
                    if token.get() != ticket {
                        return;
                    }
                    let query = text.trim().to_owned();
                    let fetcher = Arc::clone(&window.ctx.services.lyrics);
                    let found = if query.is_empty() {
                        Vec::new()
                    } else {
                        match window.ctx.services.run(async move { fetcher.lrclib.search_query(&query).await }).await {
                            Some(Ok(found)) => found,
                            Some(Err(error)) => {
                                tracing::warn!(?error, "поиск LrcLib не прошёл");
                                Vec::new()
                            }
                            None => Vec::new(),
                        }
                    };
                    if token.get() != ticket {
                        return;
                    }
                    while let Some(child) = list.first_child() {
                        list.remove(&child);
                    }
                    for track in found.iter().take(50) {
                        list.append(&result_row(&window, track, &dialog));
                    }
                    results.set_visible_child_name(if found.is_empty() { "empty" } else { "list" });
                });
            })
        };
        let changed = Rc::clone(&search);
        entry.connect_search_changed(move |entry| changed(entry.text().to_string(), Duration::from_millis(700)));
        let activated = Rc::clone(&search);
        entry.connect_activate(move |entry| activated(entry.text().to_string(), Duration::ZERO));
        search(entry.text().to_string(), Duration::ZERO);

        let (weak, closing) = (self.downgrade(), dialog.downgrade());
        import.connect_clicked(move |_| {
            if let Some(dialog) = closing.upgrade() {
                dialog.close();
            }
            if let Some(window) = weak.upgrade() {
                window.import_lyrics();
            }
        });
        dialog.present(Some(&self.window));
        entry.grab_focus();
    }

    /// Текст из файла: TTML, LRC или простой текст.
    pub fn import_lyrics(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some(tr("PlayerLyrics")));
        for pattern in ["*.lrc", "*.ttml", "*.xml", "*.txt"] {
            filter.add_pattern(pattern);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder().title(tr("LyricsImportFile")).modal(true).filters(&filters).default_filter(&filter).build();
        if let Some(folder) = glib::user_special_dir(glib::UserDirectory::Downloads) {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let weak = self.downgrade();
        dialog.open(Some(&self.window), gio::Cancellable::NONE, move |result| {
            let (Some(window), Ok(file)) = (weak.upgrade(), result) else { return };
            glib::spawn_future_local(async move {
                let text = match file.load_contents_future().await {
                    Ok((bytes, _)) => String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').to_owned(),
                    Err(error) => {
                        tracing::warn!(%error, "файл текста не прочитался");
                        window.toast(tr("LyricsImportFailed"));
                        return;
                    }
                };
                window.toast(tr(if window.lyrics.import(&text) { "LyricsImported" } else { "LyricsImportFailed" }));
            });
        });
    }
}

fn result_row(window: &MainWindow, track: &LrcLibTrack, dialog: &glib::WeakRef<adw::Dialog>) -> gtk::ListBoxRow {
    let synced = track.synced_lyrics.as_deref().is_some_and(|t| !t.trim().is_empty());
    let kind = tr(if synced { "LyricsResultSynced" } else { "LyricsResultPlain" });
    let title = format!("{} — {}", track.artist_name, track.track_name);
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&title))
        .subtitle(format!("{} · {kind}", format_duration((track.duration * 1000.0) as i64)))
        .title_lines(2)
        .activatable(true)
        .build();
    let (weak, chosen, dialog) = (window.downgrade(), track.clone(), dialog.clone());
    row.connect_activated(move |_| {
        if let Some(window) = weak.upgrade() {
            window.lyrics.use_lrclib(&chosen);
        }
        if let Some(dialog) = dialog.upgrade() {
            dialog.close();
        }
    });
    row.upcast()
}
