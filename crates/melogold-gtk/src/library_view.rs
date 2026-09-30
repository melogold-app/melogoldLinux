//! Библиотека в окне: что лайкнуто и скрыто, свои плейлисты, строки треков с ♡ и меткой «есть без
//! сети», меню трека (§5.3, REWRITE §3.11.3, Windows `TrackActions.cs`) и отложенные разрушающие
//! действия с «Отменить» (§5.6).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::music::Track;
use melogold_data::library::LocalPlaylist;
use melogold_data::Change;
use melogold_playback::downloads::DownloadState;
use melogold_playback::engine::Command;
use serde::{Deserialize, Serialize};

use crate::localization::{tr, trf};
use crate::track_row::TrackRow;
use crate::window::MainWindow;

/// Откуда показан трек: от этого зависит «Убрать из…» в меню.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RowContext {
    #[default]
    Plain,
    Playlist(i64),
    History,
    Queue(i64),
    /// Играющий трек, меню «…» панели плеера.
    Player,
}

/// Параметр действий меню трека: трек и то, над чем действие.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TrackTarget {
    pub track: Track,
    #[serde(default)]
    pub context: RowContext,
    #[serde(default)]
    pub playlist: Option<i64>,
    #[serde(default)]
    pub artist: Option<String>,
}

impl TrackTarget {
    pub fn variant(&self) -> glib::Variant {
        serde_json::to_string(self).unwrap_or_default().to_variant()
    }

    pub fn from_variant(value: Option<&glib::Variant>) -> Option<TrackTarget> {
        value.and_then(|v| v.get::<String>()).and_then(|json| serde_json::from_str(&json).ok())
    }
}

type Refresh = Weak<dyn Fn()>;

#[derive(Default)]
pub struct LibraryView {
    liked: RefCell<HashSet<String>>,
    hidden: RefCell<HashSet<String>>,
    playlists: RefCell<Vec<LocalPlaylist>>,
    /// Живые строки треков: ♡ и метки обновляются по треку, который в строке сейчас.
    rows: RefCell<Vec<glib::WeakRef<TrackRow>>>,
    listeners: RefCell<Vec<(u32, Refresh)>>,
    /// Убранное, пока идёт «Отменить»: экраны его уже не показывают, в базе оно ещё есть.
    removing: RefCell<HashSet<String>>,
    /// Отложенное разрушающее действие: выполнится через 5 с, если не нажали «Отменить».
    pending: RefCell<Option<(adw::Toast, Rc<Cell<bool>>, Box<dyn FnOnce()>)>>,
}

impl LibraryView {
    pub fn is_liked(&self, video_id: &str) -> bool {
        self.liked.borrow().contains(video_id)
    }

    pub fn is_hidden(&self, video_id: &str) -> bool {
        self.hidden.borrow().contains(video_id)
    }

    pub fn playlists(&self) -> Vec<LocalPlaylist> {
        self.playlists.borrow().clone()
    }

    pub fn register_row(&self, row: &TrackRow) {
        let mut rows = self.rows.borrow_mut();
        // Чистим от ушедших строк не каждый раз: списки создают их пачками.
        if rows.len() % 64 == 63 {
            rows.retain(|r| r.upgrade().is_some());
        }
        rows.push(row.downgrade());
    }

    pub fn live_rows(&self) -> Vec<TrackRow> {
        let mut rows = self.rows.borrow_mut();
        rows.retain(|r| r.upgrade().is_some());
        rows.iter().filter_map(glib::WeakRef::upgrade).collect()
    }

    /// Убирается ли сейчас: `removal_key` плейлиста, трека плейлиста или трека истории.
    pub fn is_removing(&self, key: &str) -> bool {
        self.removing.borrow().contains(key)
    }

    /// Экран, который обновляется при изменениях `mask` (биты [`Change`]); живёт, пока жив `refresh`.
    pub fn listen(&self, mask: Change, refresh: &Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push((mask.0, Rc::downgrade(refresh)));
    }
}

impl MainWindow {
    /// Название играющего трека — в его альбом (§5.2): панель плеера и «Сейчас играет».
    pub fn open_playing_album(&self) {
        if let Some(track) = self.state_track().filter(|t| t.album_id.is_some()) {
            let target = TrackTarget { track, context: RowContext::Player, ..Default::default() };
            let _ = WidgetExt::activate_action(&self.window, "win.track-album", Some(&target.variant()));
        }
    }

    /// Исполнитель играющего трека — на его страницу; у трека с несколькими исполнителями —
    /// меню с именами у подписи, как «Открыть исполнителя» в меню трека.
    pub fn open_playing_artist(&self, anchor: &gtk::Widget) {
        let Some(track) = self.state_track() else { return };
        let linked: Vec<_> = track.artists.iter().filter(|a| a.id.is_some()).cloned().collect();
        let target = TrackTarget { track, context: RowContext::Player, ..Default::default() };
        if linked.len() < 2 {
            let _ = WidgetExt::activate_action(&self.window, "win.track-artist", Some(&target.variant()));
            return;
        }
        let menu = gio::Menu::new();
        for artist in linked {
            let item = gio::MenuItem::new(Some(&artist.name), None);
            let target = TrackTarget { artist: artist.id.clone(), ..target.clone() };
            item.set_action_and_target_value(Some("win.track-artist"), Some(&target.variant()));
            menu.append_item(&item);
        }
        let popover = gtk::PopoverMenu::from_model(Some(&menu));
        popover.set_parent(anchor);
        popover.connect_closed(|popover| {
            let popover = popover.clone();
            glib::idle_add_local_once(move || popover.unparent());
        });
        popover.popup();
    }

    /// Первое чтение библиотеки и подписка на изменения.
    pub fn start_library(&self) {
        self.reload_library(Change(u32::MAX));
        let (changes, weak) = (self.ctx.services.library_changes.clone(), self.downgrade());
        glib::spawn_future_local(async move {
            while let Ok(change) = changes.recv().await {
                let Some(window) = weak.upgrade() else { break };
                window.reload_library(change);
            }
        });
        let (offline, weak) = (self.ctx.services.offline_changes.clone(), self.downgrade());
        glib::spawn_future_local(async move {
            while let Ok(video_id) = offline.recv().await {
                let Some(window) = weak.upgrade() else { break };
                window.update_marks(&video_id);
                window.notify_library(Change::DOWNLOADS);
            }
        });
    }

    fn reload_library(&self, change: Change) {
        let task = self.ctx.services.db(move |library| {
            let liked = change.has(Change::LIKES).then(|| library.liked_ids().unwrap_or_default());
            let hidden = change.has(Change::BLOCKS).then(|| library.hidden_tracks().unwrap_or_default());
            let playlists = change.has(Change::PLAYLISTS).then(|| library.playlists().unwrap_or_default());
            (liked, hidden, playlists)
        });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let (Some(window), Some((liked, hidden, playlists))) = (weak.upgrade(), task.await) else { return };
            let view = &window.library_view;
            if let Some(liked) = liked {
                *view.liked.borrow_mut() = liked;
                window.update_hearts();
            }
            if let Some(hidden) = hidden {
                *view.hidden.borrow_mut() = hidden;
            }
            if let Some(playlists) = playlists {
                *view.playlists.borrow_mut() = playlists;
            }
            if change.has(Change::LYRICS) {
                window.lyrics.library_changed();
            }
            if change.has(Change::OVERRIDES) {
                window.refresh_display();
            }
            window.notify_library(change);
        });
    }

    fn notify_library(&self, change: Change) {
        let listeners: Vec<Rc<dyn Fn()>> = {
            let mut listeners = self.library_view.listeners.borrow_mut();
            listeners.retain(|(_, refresh)| refresh.strong_count() > 0);
            listeners.iter().filter(|(mask, _)| mask & change.0 != 0).filter_map(|(_, r)| r.upgrade()).collect()
        };
        for refresh in listeners {
            refresh();
        }
    }

    pub fn update_hearts(&self) {
        let rows = self.library_view.live_rows();
        let liked = self.library_view.liked.borrow();
        for row in rows {
            if let Some(video_id) = row.video_id() {
                row.refresh_heart(liked.contains(&video_id));
            }
        }
        let playing = self.state_track().map(|t| liked.contains(&t.video_id)).unwrap_or(false);
        self.player_bar().set_liked(playing);
        if let Some(now_playing) = self.now_playing.get() {
            now_playing.set_liked(playing);
        }
    }

    fn update_marks(&self, video_id: &str) {
        let state = self.offline_state(video_id);
        for row in self.library_view.live_rows() {
            if row.video_id().as_deref() == Some(video_id) {
                row.refresh_mark(state);
            }
        }
    }

    /// «Есть без сети» (§5.3): скачан — закрашенный значок, в кэше — контуром, скачивается — доля, сбой.
    pub fn offline_state(&self, video_id: &str) -> Offline {
        match self.ctx.services.downloads.state(video_id) {
            Some(DownloadState::Completed) => Offline::Downloaded,
            Some(DownloadState::Downloading(progress)) => Offline::Downloading(progress),
            Some(DownloadState::Queued) => Offline::Downloading(None),
            Some(DownloadState::Failed) => Offline::Failed,
            None if self.ctx.services.songs.is_complete(video_id) => Offline::Cached,
            None => Offline::None,
        }
    }

    // ── строка трека ──

    /// Строка трека для `GtkListBox` ([`TrackRow`] внутри строки списка).
    pub fn track_row(&self, track: &Track, context: RowContext) -> gtk::ListBoxRow {
        let row = TrackRow::new(self);
        row.bind(track, context);
        gtk::ListBoxRow::builder().child(&row).build()
    }

    /// Меню трека у строки не из [`TrackRow`] (очередь): правый щелчок, клавиша меню и Shift+F10.
    pub fn attach_context_menu(&self, row: &impl IsA<gtk::Widget>, target: TrackTarget) {
        let target = Rc::new(target);
        let click = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_SECONDARY).build();
        let (weak, widget, t) = (self.downgrade(), row.clone().upcast::<gtk::Widget>(), Rc::clone(&target));
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(window) = weak.upgrade() {
                popup(&widget, &window.track_menu_for(&t), Some((x, y)));
            }
        });
        row.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let (weak, widget) = (self.downgrade(), row.clone().upcast::<gtk::Widget>());
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let menu_key =
                key == gtk::gdk::Key::Menu || (key == gtk::gdk::Key::F10 && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK));
            if !menu_key {
                return glib::Propagation::Proceed;
            }
            if let Some(window) = weak.upgrade() {
                popup(&widget, &window.track_menu_for(&target), None);
            }
            glib::Propagation::Stop
        });
        row.add_controller(keys);
    }

    // ── меню трека ──

    /// Пункты и порядок — как у всех клиентов (REWRITE §3.11.3): очередь, плейлист, загрузка, радио и
    /// ♡, переходы, ссылка; после черты — «Убрать из…» по месту и «Не показывать». У играющего трека
    /// нет «Играть следующим», «В конец очереди» и ♡ — они рядом, в панели плеера.
    pub fn track_menu_for(&self, target: &TrackTarget) -> gio::Menu {
        let track = &target.track;
        let player = target.context == RowContext::Player;
        let item = |label: &str, action: &str, extra: Option<TrackTarget>| {
            let entry = gio::MenuItem::new(Some(label), None);
            entry.set_action_and_target_value(Some(action), Some(&extra.unwrap_or_else(|| target.clone()).variant()));
            entry
        };
        let menu = gio::Menu::new();
        let first = gio::Menu::new();
        if !player {
            first.append_item(&item(tr("MenuPlayNext"), "win.track-play-next", None));
            first.append_item(&item(tr("MenuAddToQueue"), "win.track-add-to-queue", None));
        }
        let playlists = gio::Menu::new();
        let new_playlist = gio::Menu::new();
        new_playlist.append_item(&item(tr("SelectionNewPlaylist"), "win.track-new-playlist", None));
        playlists.append_section(None, &new_playlist);
        let existing = gio::Menu::new();
        for playlist in self.library_view.playlists().iter().take(30) {
            let extra = TrackTarget { playlist: Some(playlist.id), ..target.clone() };
            existing.append_item(&item(&playlist.name, "win.track-add-to-playlist", Some(extra)));
        }
        playlists.append_section(None, &existing);
        first.append_submenu(Some(tr("MenuAddToPlaylist")), &playlists);
        first.append_item(&item(tr("LinuxEditDetails"), "win.track-edit-details", None));
        // «Скачать» по состоянию загрузки (Android `DownloadEntry`).
        if track.is_live() {
            first.append_item(&item(tr("MenuDownloadLive"), "win.disabled", None));
        } else {
            let (label, action) = match self.ctx.services.downloads.state(&track.video_id) {
                None => (tr("MenuDownload").to_owned(), "win.track-download"),
                Some(DownloadState::Completed) => (tr("MenuDownloadRemove").to_owned(), "win.track-download-remove"),
                Some(DownloadState::Failed) => (tr("MenuDownloadRetry").to_owned(), "win.track-download"),
                Some(DownloadState::Downloading(Some(progress))) => {
                    (trf("MenuDownloadCancelFormat", &[&((progress * 100.0) as i64)]), "win.track-download-cancel")
                }
                Some(_) => (tr("MenuDownloadCancel").to_owned(), "win.track-download-cancel"),
            };
            first.append_item(&item(&label, action, None));
        }
        first.append_item(&item(tr("MenuSaveFile"), "win.track-save-file", None));
        menu.append_section(None, &first);

        let second = gio::Menu::new();
        second.append_item(&item(tr("MenuTrackRadio"), "win.track-radio", None));
        if !player {
            let key = if self.library_view.is_liked(&track.video_id) { "MenuFavoriteRemove" } else { "MenuFavoriteAdd" };
            second.append_item(&item(tr(key), "win.track-like", None));
        }
        menu.append_section(None, &second);

        let third = gio::Menu::new();
        if track.album_id.is_some() {
            third.append_item(&item(tr("MenuGoToAlbum"), "win.track-album", None));
        }
        let artist_key = if track.is_video() { "MenuGoToChannel" } else { "MenuGoToArtist" };
        let linked: Vec<_> = track.artists.iter().filter(|a| a.id.is_some()).collect();
        match linked.as_slice() {
            [] if track.artists_text.as_deref().is_some_and(|t| !t.trim().is_empty()) => {
                third.append_item(&item(tr(artist_key), "win.track-artist", None));
            }
            [] => {}
            [only] => third.append_item(&item(
                tr(artist_key),
                "win.track-artist",
                Some(TrackTarget { artist: only.id.clone(), ..target.clone() }),
            )),
            many => {
                let sub = gio::Menu::new();
                for artist in many {
                    sub.append_item(&item(
                        &artist.name,
                        "win.track-artist",
                        Some(TrackTarget { artist: artist.id.clone(), ..target.clone() }),
                    ));
                }
                third.append_submenu(Some(tr(artist_key)), &sub);
            }
        }
        third.append_item(&item(tr("MenuOtherVersions"), "win.track-other-versions", None));
        third.append_item(&item(tr("LinuxCopyLink"), "win.track-copy-link", None));
        menu.append_section(None, &third);

        let removals = gio::Menu::new();
        let remove_key = match target.context {
            RowContext::Playlist(_) => Some("MenuRemoveFromPlaylist"),
            RowContext::History => Some("MenuRemoveFromHistory"),
            RowContext::Queue(_) => Some("MenuRemoveFromQueue"),
            _ => None,
        };
        if let Some(key) = remove_key {
            removals.append_item(&item(tr(key), "win.track-remove", None));
        }
        let hide_key = if self.library_view.is_hidden(&track.video_id) { "MenuShowAgain" } else { "MenuDontShow" };
        removals.append_item(&item(tr(hide_key), "win.track-hide", None));
        menu.append_section(None, &removals);
        menu
    }

    // ── действия ──

    /// Действия меню трека: параметр — [`TrackTarget`] строкой JSON.
    pub fn install_track_actions(&self) {
        let with_target = |name: &str, run: fn(&MainWindow, TrackTarget)| {
            let weak = self.downgrade();
            gio::ActionEntry::builder(name)
                .parameter_type(Some(glib::VariantTy::STRING))
                .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                    if let (Some(window), Some(target)) = (weak.upgrade(), TrackTarget::from_variant(parameter)) {
                        run(&window, target);
                    }
                })
                .build()
        };
        let entries = [
            with_target("track-play-next", |w, t| {
                w.toast(&trf("PlayingNextFormat", &[&t.track.title]));
                w.ctx.services.player.send(Command::PlayNext(vec![t.track]));
            }),
            with_target("track-add-to-queue", |w, t| {
                w.toast(&trf("QueuedCountFormat", &[&t.track.title]));
                w.ctx.services.player.send(Command::AddToEnd(vec![t.track]));
            }),
            with_target("track-new-playlist", |w, t| w.new_playlist(vec![t.track])),
            with_target("track-add-to-playlist", |w, t| {
                if let Some(playlist) = t.playlist {
                    w.add_to_playlist(playlist, vec![t.track]);
                }
            }),
            with_target("track-download", |w, t| {
                if matches!(w.ctx.services.downloads.state(&t.track.video_id), Some(DownloadState::Failed)) {
                    w.ctx.services.downloads.retry(&t.track.video_id);
                } else {
                    w.ctx.services.downloads.download(&[t.track]);
                }
            }),
            with_target("track-download-remove", |w, t| {
                let downloads = w.ctx.services.downloads.clone();
                w.undoable(tr("DownloadRemoved"), move || downloads.remove(&t.track.video_id), || {});
            }),
            with_target("track-download-cancel", |w, t| w.ctx.services.downloads.remove(&t.track.video_id)),
            with_target("track-save-file", |w, t| w.save_file(t.track)),
            with_target("track-radio", |w, t| w.ctx.services.player.send(Command::PlaySingle { track: t.track, start: Duration::ZERO })),
            with_target("track-like", |w, t| {
                let liked = !w.library_view.is_liked(&t.track.video_id);
                w.set_liked(vec![t.track], liked);
            }),
            with_target("track-album", |w, t| {
                if let Some(album) = &t.track.album_id {
                    w.push(&crate::pages::catalog::album_page(w, album));
                }
            }),
            with_target("track-artist", |w, t| match t.artist.or_else(|| t.track.artists.iter().find_map(|a| a.id.clone())) {
                Some(id) => w.push(&crate::pages::catalog::artist_page(w, &id)),
                // Трек без ссылок (из истории, из файла): исполнитель находится поиском по имени.
                None => w.search_for(t.track.artists_text.as_deref().unwrap_or_default()),
            }),
            with_target("track-other-versions", |w, t| w.other_versions(&t.track)),
            with_target("track-copy-link", |w, t| w.copy_link(&t.track)),
            with_target("track-remove", |w, t| match t.context {
                RowContext::Playlist(id) => w.remove_from_playlist(id, t.track),
                RowContext::History => w.remove_from_history(t.track),
                RowContext::Queue(id) => w.ctx.services.player.send(Command::Remove(id)),
                RowContext::Plain | RowContext::Player => {}
            }),
            with_target("track-hide", |w, t| w.toggle_hidden(t.track, t.context == RowContext::Player)),
            with_target("track-edit-details", |w, t| w.edit_details(&t.track)),
        ];
        self.window.add_action_entries(entries);
        // Пункт-объяснение («У трансляции нечего скачивать»): виден, но не нажимается.
        let disabled = gio::SimpleAction::new("disabled", Some(glib::VariantTy::STRING));
        disabled.set_enabled(false);
        self.window.add_action(&disabled);

        let with_playlist = |name: &str, run: fn(&MainWindow, i64)| {
            let weak = self.downgrade();
            gio::ActionEntry::builder(name)
                .parameter_type(Some(glib::VariantTy::INT64))
                .activate(move |_: &adw::ApplicationWindow, _, parameter| {
                    if let (Some(window), Some(id)) = (weak.upgrade(), parameter.and_then(|p| p.get::<i64>())) {
                        run(&window, id);
                    }
                })
                .build()
        };
        self.window.add_action_entries([
            with_playlist("playlist-rename", |w, id| w.rename_playlist(id)),
            with_playlist("playlist-copy-link", |w, id| w.share_local_playlist(id)),
            with_playlist("playlist-delete", |w, id| w.delete_playlist(id)),
        ]);
    }

    /// «Сведения о треке» (задание 0005): свои название, исполнитель и альбом. В пустом поле серым —
    /// как на YouTube; «Как на YouTube» снимает правку целиком.
    pub fn edit_details(&self, track: &Track) {
        let library = std::sync::Arc::clone(&self.ctx.services.library);
        let current = library.track_override(&track.video_id).unwrap_or_default();
        let dialog = adw::AlertDialog::new(Some(tr("LinuxTrackDetails")), Some(tr("LinuxTrackDetailsHint")));
        let fields = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
        let field = |label: &str, value: Option<&str>, youtube: Option<&str>| {
            let caption = gtk::Label::builder().label(label).xalign(0.0).build();
            caption.add_css_class("caption-heading");
            let entry = gtk::Entry::builder()
                .text(value.unwrap_or_default())
                .placeholder_text(youtube.unwrap_or_default())
                .activates_default(true)
                .build();
            entry.update_property(&[gtk::accessible::Property::Label(label)]);
            let group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
            group.append(&caption);
            group.append(&entry);
            fields.append(&group);
            entry
        };
        let title = field(tr("LinuxTrackDetailsName"), current.title.as_deref(), Some(&track.title));
        let artist = field(tr("LinuxTrackDetailsArtist"), current.artists_text.as_deref(), track.artists_text.as_deref());
        let album = field(tr("LinuxTrackDetailsAlbum"), current.album_title.as_deref(), track.album_title.as_deref());
        dialog.set_extra_child(Some(&fields));
        dialog.add_response("reset", tr("LinuxTrackDetailsReset"));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("save", tr("SaveAsPlaylist"));
        dialog.set_response_enabled("reset", !current.is_empty());
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let video_id = track.video_id.clone();
        dialog.connect_response(None, move |_, response| {
            let texts = match response {
                "save" => [title.text().to_string(), artist.text().to_string(), album.text().to_string()],
                "reset" => Default::default(),
                _ => return,
            };
            let (library, video_id) = (std::sync::Arc::clone(&library), video_id.clone());
            // Запись — не в главном потоке; экраны обновит событие библиотеки.
            std::thread::spawn(move || {
                let field = |text: &str| Some(text.to_owned()).filter(|t| !t.trim().is_empty());
                let [t, a, al] = texts;
                if let Err(error) = library.set_override(&video_id, field(&t).as_deref(), field(&a).as_deref(), field(&al).as_deref()) {
                    tracing::warn!(%error, "правка трека не записалась");
                }
            });
        });
        dialog.present(Some(&self.window));
    }

    /// «Указать альбом…» выделенному (задание 0005): по умолчанию — общий альбом выделенного, иначе
    /// название своего плейлиста, из которого выделяли.
    pub fn set_album(&self, tracks: Vec<Track>, place: RowContext) {
        let shown: Vec<Track> = tracks.iter().map(|t| self.display(t)).collect();
        let mut initial = suggested_playlist_name(&shown);
        if initial.is_empty() {
            if let RowContext::Playlist(id) = place {
                initial = self.library_view.playlists().into_iter().find(|p| p.id == id).map(|p| p.name).unwrap_or_default();
            }
        }
        let weak = self.downgrade();
        self.ask_text(tr("LinuxSetAlbumTitle"), tr("LinuxTrackDetailsAlbum"), &initial, move |album| {
            let Some(window) = weak.upgrade() else { return };
            let ids: Vec<String> = tracks.iter().map(|t| t.video_id.clone()).collect();
            let text = album.clone();
            let task = window.ctx.services.db(move |library| library.set_album(&ids, &text));
            let weak = window.downgrade();
            glib::spawn_future_local(async move {
                if let (Some(window), Some(Ok(()))) = (weak.upgrade(), task.await) {
                    window.toast(&trf("LinuxAlbumSetFormat", &[&album]));
                }
            });
        });
    }

    /// «Другие версии»: тот же трек у других загрузчиков — поиск по исполнителю и названию.
    pub fn other_versions(&self, track: &Track) {
        let query = format!("{} {}", track.artists_text.as_deref().unwrap_or_default(), track.title);
        self.search_for(query.trim());
    }

    /// «Убрать из плейлиста» (§5.6): строка пропадает сразу, из базы — через 5 с, если не нажали «Отменить».
    /// Alt+↑ ↓: трек на место выше или ниже в своём плейлисте; фокус остаётся на нём.
    pub fn move_in_playlist(&self, playlist: i64, video_id: &str, delta: i64) {
        let id = video_id.to_owned();
        let task = self.ctx.services.db(move |library| {
            let order: Vec<String> = library.playlist_tracks(playlist).unwrap_or_default().into_iter().map(|t| t.video_id).collect();
            let Some(index) = order.iter().position(|v| *v == id) else { return false };
            let target = (index as i64 + delta).clamp(0, order.len().saturating_sub(1) as i64) as usize;
            target != index && library.move_in_playlist(playlist, &id, target).is_ok()
        });
        let (weak, video_id) = (self.downgrade(), video_id.to_owned());
        glib::spawn_future_local(async move {
            if task.await != Some(true) {
                return;
            }
            // Список перерисуется по изменению библиотеки — фокус догоняет трек.
            glib::timeout_future(std::time::Duration::from_millis(150)).await;
            let Some(window) = weak.upgrade() else { return };
            if let Some(row) =
                window.library_view.live_rows().into_iter().find(|r| r.is_mapped() && r.video_id().as_deref() == Some(&video_id))
            {
                row.grab_focus();
            }
        });
    }

    fn remove_from_playlist(&self, playlist: i64, track: Track) {
        let key = removal_key_track(playlist, &track.video_id);
        self.library_view.removing.borrow_mut().insert(key.clone());
        self.notify_library(Change::PLAYLISTS);
        let (weak, undo_weak, undo_key) = (self.downgrade(), self.downgrade(), key.clone());
        self.undoable(
            &trf("RemovedFromPlaylistFormat", &[&track.title]),
            move || {
                let Some(window) = weak.upgrade() else { return };
                let task = window.ctx.services.db(move |library| library.remove_from_playlist(playlist, &track.video_id));
                glib::spawn_future_local(async move {
                    let _ = task.await;
                    if let Some(window) = weak.upgrade() {
                        window.library_view.removing.borrow_mut().remove(&key);
                    }
                });
            },
            move || {
                if let Some(window) = undo_weak.upgrade() {
                    window.library_view.removing.borrow_mut().remove(&undo_key);
                    window.notify_library(Change::PLAYLISTS);
                }
            },
        );
    }

    /// «Убрать из истории»: граница — момент нажатия, прослушивания за время отсрочки останутся.
    fn remove_from_history(&self, track: Track) {
        let key = removal_key_history(&track.video_id);
        let before = melogold_core::text::now_ms();
        self.library_view.removing.borrow_mut().insert(key.clone());
        self.notify_library(Change::HISTORY);
        let (weak, undo_weak, undo_key) = (self.downgrade(), self.downgrade(), key.clone());
        self.undoable(
            // С аккаунтом трек уходит из Истории на всех устройствах — так и сказать.
            &trf(
                if self.ctx.services.account.session().is_some() {
                    "RemovedFromHistoryEverywhereFormat"
                } else {
                    "RemovedFromHistoryFormat"
                },
                &[&track.title],
            ),
            move || {
                let Some(window) = weak.upgrade() else { return };
                let task = window.ctx.services.db(move |library| library.remove_from_history(&track.video_id, Some(before)));
                glib::spawn_future_local(async move {
                    let _ = task.await;
                    if let Some(window) = weak.upgrade() {
                        window.library_view.removing.borrow_mut().remove(&key);
                    }
                });
            },
            move || {
                if let Some(window) = undo_weak.upgrade() {
                    window.library_view.removing.borrow_mut().remove(&undo_key);
                    window.notify_library(Change::HISTORY);
                }
            },
        );
    }

    /// «Не показывать» (REWRITE §3.11.3): сразу, с «Отменить»; играющий трек пропускается.
    fn toggle_hidden(&self, track: Track, playing: bool) {
        let hide = !self.library_view.is_hidden(&track.video_id);
        if hide {
            self.library_view.hidden.borrow_mut().insert(track.video_id.clone());
        } else {
            self.library_view.hidden.borrow_mut().remove(&track.video_id);
        }
        let current = self.state_track().is_some_and(|t| t.video_id == track.video_id);
        let task_track = track.clone();
        let task = self.ctx.services.db(move |library| library.set_track_hidden(&task_track, hide));
        glib::spawn_future_local(async move {
            if let Some(Err(error)) = task.await {
                tracing::warn!(%error, "«Не показывать» не записалось");
            }
        });
        if !hide {
            return;
        }
        if playing || current {
            self.ctx.services.player.send(Command::Next);
        }
        let toast = adw::Toast::builder().title(tr("TrackHidden")).button_label(tr("Undo")).timeout(5).build();
        let weak = self.downgrade();
        toast.connect_button_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.toggle_hidden(track.clone(), false);
            }
        });
        self.add_toast(toast);
    }

    fn rename_playlist(&self, id: i64) {
        let Some(playlist) = self.library_view.playlists().into_iter().find(|p| p.id == id) else { return };
        let weak = self.downgrade();
        self.ask_name(tr("Rename"), &playlist.name, move |name| {
            let Some(window) = weak.upgrade() else { return };
            let task = window.ctx.services.db(move |library| library.rename_playlist(id, &name));
            glib::spawn_future_local(async move {
                if let Some(Err(error)) = task.await {
                    tracing::warn!(%error, "плейлист не переименовался");
                }
            });
        });
    }

    /// «Удалить плейлист»: страница закрывается, плейлист пропадает из Библиотеки; «Отменить» возвращает его.
    fn delete_playlist(&self, id: i64) {
        let Some(playlist) = self.library_view.playlists().into_iter().find(|p| p.id == id) else { return };
        let key = removal_key_playlist(id);
        self.library_view.removing.borrow_mut().insert(key.clone());
        let nav = self.nav(melogold_core::settings::Tab::Library);
        if nav.visible_page().is_some_and(|p| p.tag().as_deref() == Some(key.as_str())) {
            nav.pop();
        }
        self.notify_library(Change::PLAYLISTS);
        let (weak, undo_weak, undo_key) = (self.downgrade(), self.downgrade(), key.clone());
        self.undoable(
            &trf("PlaylistDeletedFormat", &[&playlist.name]),
            move || {
                let Some(window) = weak.upgrade() else { return };
                let task = window.ctx.services.db(move |library| library.delete_playlist(id));
                glib::spawn_future_local(async move {
                    let _ = task.await;
                    if let Some(window) = weak.upgrade() {
                        window.library_view.removing.borrow_mut().remove(&key);
                    }
                });
            },
            move || {
                if let Some(window) = undo_weak.upgrade() {
                    window.library_view.removing.borrow_mut().remove(&undo_key);
                    window.notify_library(Change::PLAYLISTS);
                    window.push(&crate::pages::library::local_playlist(&window, id));
                }
            },
        );
    }

    pub fn set_liked(&self, tracks: Vec<Track>, liked: bool) {
        {
            let mut set = self.library_view.liked.borrow_mut();
            for track in &tracks {
                if liked {
                    set.insert(track.video_id.clone());
                } else {
                    set.remove(&track.video_id);
                }
            }
        }
        self.update_hearts();
        let task = self.ctx.services.db(move |library| library.set_liked(&tracks, liked));
        glib::spawn_future_local(async move {
            if let Some(Err(error)) = task.await {
                tracing::warn!(%error, "♡ не записался");
            }
        });
    }

    pub fn add_to_playlist(&self, playlist: i64, tracks: Vec<Track>) {
        let name = self.library_view.playlists().into_iter().find(|p| p.id == playlist).map(|p| p.name).unwrap_or_default();
        let task = self.ctx.services.db(move |library| library.add_to_playlist(playlist, &tracks));
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            if let (Some(window), Some(Ok(_))) = (weak.upgrade(), task.await) {
                window.toast(&trf("AddedToPlaylistFormat", &[&name]));
            }
        });
    }

    /// «Новый плейлист…»: название по умолчанию — общий альбом треков, если он у всех один (задание 0004).
    pub fn new_playlist(&self, tracks: Vec<Track>) {
        let suggested = suggested_playlist_name(&tracks);
        let weak = self.downgrade();
        self.ask_name(tr("NewPlaylist"), &suggested, move |name| {
            let Some(window) = weak.upgrade() else { return };
            let count = tracks.len() as i64;
            let (task_tracks, task_name) = (tracks.clone(), name.clone());
            let task = window.ctx.services.db(move |library| library.create_playlist(&task_name, &task_tracks));
            let weak = window.downgrade();
            glib::spawn_future_local(async move {
                let (Some(window), Some(Ok(id))) = (weak.upgrade(), task.await) else { return };
                let toast = adw::Toast::builder()
                    .title(trf("PlaylistCreatedFormat", &[&name, &crate::localization::plural("Tracks", count)]))
                    .button_label(tr("OpenAction"))
                    .build();
                let weak = window.downgrade();
                toast.connect_button_clicked(move |_| {
                    if let Some(window) = weak.upgrade() {
                        window.push(&crate::pages::library::local_playlist(&window, id));
                    }
                });
                window.add_toast(toast);
            });
        });
    }

    /// Диалог с одним полем названия.
    pub fn ask_name(&self, title: &str, initial: &str, done: impl Fn(String) + 'static) {
        self.ask_text(title, tr("NewPlaylistName"), initial, done);
    }

    /// Диалог с одним полем: `field` — подпись поля, пустое не сохраняется.
    pub fn ask_text(&self, title: &str, field: &str, initial: &str, done: impl Fn(String) + 'static) {
        let dialog = adw::AlertDialog::new(Some(title), None);
        let entry = adw::EntryRow::builder().title(field).text(initial).activates_default(true).build();
        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        list.append(&entry);
        dialog.set_extra_child(Some(&list));
        dialog.add_response("cancel", tr("Cancel"));
        dialog.add_response("ok", tr("SaveAsPlaylist"));
        dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response("cancel");
        // Пустое название не сохраняется: кнопка ждёт хотя бы одну букву.
        dialog.set_response_enabled("ok", !initial.trim().is_empty());
        let weak_dialog = dialog.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_response_enabled("ok", !entry.text().trim().is_empty());
            }
        });
        dialog.connect_response(None, move |_, response| {
            let name = entry.text().trim().to_owned();
            if response == "ok" && !name.is_empty() {
                done(name);
            }
        });
        dialog.present(Some(&self.window));
    }

    /// Разрушающее действие (§5.6): выполняется через 5 с, плашка с «Отменить». Плашка одна: новая сразу
    /// выполняет прежнее действие. Граница действия — момент нажатия, а не конец отсрочки.
    pub fn undoable(&self, title: &str, commit: impl FnOnce() + 'static, undo: impl FnOnce() + 'static) {
        if let Some((toast, undone, previous)) = self.library_view.pending.take() {
            undone.set(true);
            toast.dismiss();
            previous();
        }
        let toast = adw::Toast::builder().title(title).button_label(tr("Undo")).timeout(5).build();
        let undone = Rc::new(Cell::new(false));
        let undo = RefCell::new(Some(undo));
        let flag = Rc::clone(&undone);
        toast.connect_button_clicked(move |_| {
            flag.set(true);
            if let Some(undo) = undo.take() {
                undo();
            }
        });
        let weak = self.downgrade();
        let flag = Rc::clone(&undone);
        toast.connect_dismissed(move |toast| {
            let Some(window) = weak.upgrade() else { return };
            let pending = window.library_view.pending.take();
            match pending {
                Some((current, _, commit)) if &current == toast => {
                    if !flag.get() {
                        commit();
                    }
                }
                other => {
                    window.library_view.pending.replace(other);
                }
            }
        });
        self.library_view.pending.replace(Some((toast.clone(), undone, Box::new(commit))));
        self.add_toast(toast);
    }

    /// Выход: отложенное действие выполняется сейчас, а не теряется.
    pub fn flush_pending(&self) {
        if let Some((_, undone, commit)) = self.library_view.pending.take() {
            if !undone.get() {
                commit();
            }
        }
    }
}

/// Меню у виджета: у указателя или, с клавиатуры, у самого виджета.
pub fn popup(anchor: &gtk::Widget, menu: &gio::Menu, at: Option<(f64, f64)>) {
    let popover = gtk::PopoverMenu::from_model(Some(menu));
    popover.set_parent(anchor);
    popover.set_has_arrow(false);
    if let Some((x, y)) = at {
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
    }
    popover.connect_closed(|popover| {
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });
    popover.popup();
}

/// Название нового плейлиста по умолчанию: альбом выделенного, если он у всех, у кого альбом
/// есть, один (задание 0004, как Windows: у видео альбома нет, и они его не отменяют).
pub fn suggested_playlist_name(tracks: &[Track]) -> String {
    let albums: HashSet<&str> = tracks.iter().filter_map(|t| t.album_title.as_deref().map(str::trim)).filter(|a| !a.is_empty()).collect();
    if albums.len() == 1 {
        albums.into_iter().next().unwrap_or_default().to_owned()
    } else {
        String::new()
    }
}

/// Ключи убираемого: страницы прячут его, пока идёт «Отменить».
pub fn removal_key_playlist(id: i64) -> String {
    format!("playlist-{id}")
}

pub fn removal_key_track(playlist: i64, video_id: &str) -> String {
    format!("playlist-{playlist}/{video_id}")
}

pub fn removal_key_history(video_id: &str) -> String {
    format!("history/{video_id}")
}

/// Состояние «есть без сети».
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Offline {
    None,
    Cached,
    Downloaded,
    Downloading(Option<f64>),
    Failed,
}

pub fn show_mark(stack: &gtk::Stack, state: Offline) {
    let name = match state {
        Offline::None => "none",
        Offline::Cached => "cached",
        Offline::Downloaded => "downloaded",
        Offline::Downloading(_) => "downloading",
        Offline::Failed => "failed",
    };
    stack.set_visible_child_name(name);
    if let (Offline::Downloading(progress), Some(area)) = (state, stack.child_by_name("downloading").and_downcast::<gtk::DrawingArea>()) {
        let share = progress.unwrap_or(0.05).clamp(0.05, 1.0);
        // Кольцо с долей: цвет — текущий цвет текста, как у символических значков.
        area.set_draw_func(move |area, cr, width, height| {
            let color = area.color();
            let (cx, cy, r) = (f64::from(width) / 2.0, f64::from(height) / 2.0, f64::from(width.min(height)) / 2.0 - 2.0);
            cr.set_line_width(2.0);
            cr.set_source_rgba(f64::from(color.red()), f64::from(color.green()), f64::from(color.blue()), 0.25);
            cr.arc(cx, cy, r, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
            cr.set_source_rgba(f64::from(color.red()), f64::from(color.green()), f64::from(color.blue()), 1.0);
            cr.arc(cx, cy, r, -std::f64::consts::FRAC_PI_2, -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * share);
            let _ = cr.stroke();
        });
        area.queue_draw();
    }
}

pub fn set_heart(button: &gtk::Button, liked: bool) {
    button.set_icon_name(if liked { "heart-filled-symbolic" } else { "heart-outline-symbolic" });
    if liked {
        button.add_css_class("liked");
    } else {
        button.remove_css_class("liked");
    }
    let label = tr(if liked { "MenuFavoriteRemove" } else { "MenuFavoriteAdd" });
    button.update_property(&[gtk::accessible::Property::Label(label)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(album: Option<&str>) -> Track {
        Track { video_id: "v".into(), title: "t".into(), album_title: album.map(str::to_owned), ..Default::default() }
    }

    #[test]
    fn new_playlist_is_named_after_the_common_album() {
        assert_eq!(suggested_playlist_name(&[track(Some("Альбом")), track(Some(" Альбом "))]), "Альбом");
        assert_eq!(suggested_playlist_name(&[track(Some("Альбом")), track(Some("Другой"))]), "");
        // Видео без альбома общий альбом не отменяет.
        assert_eq!(suggested_playlist_name(&[track(Some("Альбом")), track(None)]), "Альбом");
        assert_eq!(suggested_playlist_name(&[track(None), track(Some(""))]), "");
        assert_eq!(suggested_playlist_name(&[]), "");
    }
}
