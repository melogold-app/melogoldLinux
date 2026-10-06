//! Меню коллекции — альбома, плейлиста YouTube, исполнителя (задание 0024; Windows `CollectionMenu`).
//!
//! Одно и то же меню у «…» в шапке коллекции и у её карточки на полках и в сетках (правый щелчок, клавиша
//! меню, Shift+F10) — доктрина §4.3 и §4.4. Порядок как у Windows: «Слушать», «Перемешать», «Играть
//! следующим», «В конец очереди»; «Сохранить в библиотеку» («Подписаться» у исполнителя) — флажком;
//! «Открыть исполнителя» у альбома; «Копировать ссылку».
//!
//! Пункты, которым нужны треки коллекции, загружают их тем же путём, что «Слушать» на её странице: альбом
//! и плейлист — свои треки, исполнитель — все его песни, не вышло — популярные. Состояние флажка известно
//! до открытия меню (грабли §9 п. 9): у карточки меню открывается, когда база ответила.

use adw::prelude::*;
use gtk::{gio, glib};
use melogold_core::music::{MusicItem, Track};
use melogold_core::share_links;
use melogold_playback::engine::Command;

use crate::localization::tr;
use crate::window::MainWindow;

pub struct CollectionMenu {
    pub model: gio::Menu,
    pub actions: gio::SimpleActionGroup,
    saved: Option<gio::SimpleAction>,
}

/// Имя группы действий меню: пункты зовут `collection.<действие>`.
pub const GROUP: &str = "collection";

impl CollectionMenu {
    /// Флажок «Сохранено» / «Подписан» — без записи в базу (состояние прочитано из неё).
    pub fn set_saved(&self, on: bool) {
        if let Some(action) = &self.saved {
            action.set_state(&on.to_variant());
        }
    }

    /// Повесить действия на `widget`: меню, открытое от него, находит их по имени группы.
    pub fn attach(&self, widget: &impl IsA<gtk::Widget>) {
        widget.insert_action_group(GROUP, Some(&self.actions));
    }
}

/// Что делать с треками коллекции.
#[derive(Clone, Copy)]
enum Use {
    Play,
    Shuffle,
    Next,
    End,
}

/// Меню коллекции `item`; у трека и настроения его нет (у трека — меню трека).
pub fn build(window: &MainWindow, item: &MusicItem) -> Option<CollectionMenu> {
    build_with(window, item, true)
}

/// `with_saved: false` — без флажка «Сохранено»: шапка исполнителя держит его кнопкой рядом, и два
/// переключателя одного состояния разъезжались бы.
pub fn build_with(window: &MainWindow, item: &MusicItem, with_saved: bool) -> Option<CollectionMenu> {
    if matches!(item, MusicItem::Track(_) | MusicItem::Mood(_)) {
        return None;
    }
    let actions = gio::SimpleActionGroup::new();
    let model = gio::Menu::new();

    let queue = gio::Menu::new();
    for (name, label, using) in [
        ("play", tr("PlayAll"), Use::Play),
        ("shuffle", tr("Shuffle"), Use::Shuffle),
        ("play-next", tr("MenuPlayNext"), Use::Next),
        ("add-to-queue", tr("MenuAddToQueue"), Use::End),
    ] {
        let action = gio::SimpleAction::new(name, None);
        let (weak, item) = (window.downgrade(), item.clone());
        action.connect_activate(move |_, _| {
            if let Some(window) = weak.upgrade() {
                with_tracks(&window, &item, using);
            }
        });
        actions.add_action(&action);
        queue.append(Some(label), Some(&format!("{GROUP}.{name}")));
    }
    model.append_section(None, &queue);

    let place = gio::Menu::new();
    let saved = match item {
        MusicItem::Album(_) | MusicItem::Artist(_) if with_saved => {
            let action = gio::SimpleAction::new_stateful("saved", None, &false.to_variant());
            let (weak, target) = (window.downgrade(), item.clone());
            action.connect_activate(move |action, _| {
                let on = !action.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
                action.set_state(&on.to_variant());
                if let Some(window) = weak.upgrade() {
                    save(&window, &target, on);
                }
            });
            actions.add_action(&action);
            let label = if matches!(item, MusicItem::Artist(_)) { tr("Subscribe") } else { tr("SaveToLibrary") };
            place.append(Some(label), Some(&format!("{GROUP}.saved")));
            Some(action)
        }
        _ => None,
    };
    if let MusicItem::Album(album) = item {
        if let Some(artist) = album.artists.iter().find_map(|a| a.id.clone()) {
            let action = gio::SimpleAction::new("open-artist", None);
            let weak = window.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(window) = weak.upgrade() {
                    window.push(&crate::pages::catalog::artist_page(&window, &artist));
                }
            });
            actions.add_action(&action);
            place.append(Some(tr("MenuGoToArtist")), Some(&format!("{GROUP}.open-artist")));
        }
    }
    let link = match item {
        MusicItem::Album(album) => Some(share_links::album_url(&album.browse_id)),
        MusicItem::Artist(artist) => Some(share_links::artist_url(&artist.browse_id, artist.is_channel)),
        MusicItem::Playlist(playlist) => Some(share_links::playlist_url(&playlist.playlist_id)),
        _ => None,
    };
    if let Some(link) = link {
        let action = gio::SimpleAction::new("copy-link", None);
        let weak = window.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(window) = weak.upgrade() {
                window.copy_link_text(&link, None);
            }
        });
        actions.add_action(&action);
        place.append(Some(tr("LinuxCopyLink")), Some(&format!("{GROUP}.copy-link")));
    }
    if place.n_items() > 0 {
        model.append_section(None, &place);
    }
    Some(CollectionMenu { model, actions, saved })
}

/// Прочитать из базы, сохранена ли коллекция, и отдать ответ `ready` (в главном потоке).
pub fn read_saved(window: &MainWindow, item: &MusicItem, ready: impl FnOnce(bool) + 'static) {
    let item = item.clone();
    let task = window.ctx.services.db(move |library| match &item {
        MusicItem::Album(album) => library.is_album_saved(&album.browse_id).unwrap_or(false),
        MusicItem::Artist(artist) => library.is_artist_saved(&artist.browse_id).unwrap_or(false),
        _ => false,
    });
    glib::spawn_future_local(async move {
        ready(task.await.unwrap_or(false));
    });
}

fn save(window: &MainWindow, item: &MusicItem, on: bool) {
    let item = item.clone();
    let task = window.ctx.services.db(move |library| match &item {
        MusicItem::Album(album) => library.set_album_saved(album, on),
        MusicItem::Artist(artist) => library.set_artist_saved(artist, on),
        _ => Ok(()),
    });
    glib::spawn_future_local(async move {
        if let Some(Err(error)) = task.await {
            tracing::warn!(%error, "коллекция не сохранилась");
        }
    });
}

/// Загрузить треки коллекции и сделать с ними `using`. Ошибка сети — всплывающее сообщение.
fn with_tracks(window: &MainWindow, item: &MusicItem, using: Use) {
    let music = window.ctx.services.music.clone();
    let item = item.clone();
    let task = window.ctx.services.run(async move {
        match item {
            MusicItem::Album(album) => Ok(music.album(&album.browse_id).await?.tracks),
            MusicItem::Playlist(playlist) => music.playlist_tracks(&playlist.playlist_id, 500).await,
            MusicItem::Artist(artist) => {
                let details = music.artist(&artist.browse_id).await?;
                if let Some(songs) = &details.songs_playlist_id {
                    if let Ok(tracks) = music.playlist_tracks(songs, 500).await {
                        if !tracks.is_empty() {
                            return Ok(tracks);
                        }
                    }
                }
                Ok(details.shelves.first().map(|shelf| shelf.tracks().cloned().collect()).unwrap_or_default())
            }
            _ => Ok(Vec::new()),
        }
    });
    let weak = window.downgrade();
    glib::spawn_future_local(async move {
        let result: Option<Result<Vec<Track>, melogold_innertube::YouTubeError>> = task.await;
        let Some(window) = weak.upgrade() else { return };
        let tracks = match result {
            Some(Ok(tracks)) if !tracks.is_empty() => tracks,
            Some(Err(error)) if error.kind == melogold_innertube::ErrorKind::Blocked => return window.toast(tr("ErrorBlocked")),
            _ => return window.toast(tr("ErrorOffline")),
        };
        let player = &window.ctx.services.player;
        match using {
            Use::Play => player.send(Command::PlayList { tracks, start: 0, shuffle: false }),
            Use::Shuffle => player.send(Command::PlayList { tracks, start: 0, shuffle: true }),
            Use::Next => player.send(Command::PlayNext(tracks)),
            Use::End => player.send(Command::AddToEnd(tracks)),
        }
    });
}

/// Меню у карточки: правый щелчок, клавиша меню, Shift+F10 — меню коллекции, у трека — меню трека.
pub fn install_on_card(window: &MainWindow, card: &gtk::Button, item: &MusicItem) {
    let popup = {
        let (weak, item, anchor) = (window.downgrade(), item.clone(), card.downgrade());
        move |at: Option<(f64, f64)>| {
            let (Some(window), Some(anchor)) = (weak.upgrade(), anchor.upgrade()) else { return };
            if let MusicItem::Track(track) = &item {
                let target = crate::library_view::TrackTarget { track: track.clone(), ..Default::default() };
                crate::library_view::popup(anchor.upcast_ref(), &window.track_menu_for(&target), at);
                return;
            }
            let Some(menu) = build(&window, &item) else { return };
            menu.attach(&anchor);
            let menu = std::rc::Rc::new(menu);
            read_saved(&window, &item, move |on| {
                menu.set_saved(on);
                crate::library_view::popup(anchor.upcast_ref(), &menu.model, at);
            });
        }
    };
    let click = gtk::GestureClick::builder().button(gtk::gdk::BUTTON_SECONDARY).build();
    {
        let popup = popup.clone();
        click.connect_pressed(move |gesture, _, x, y| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            popup(Some((x, y)));
        });
    }
    card.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        let menu_key = key == gtk::gdk::Key::Menu || (key == gtk::gdk::Key::F10 && modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK));
        if menu_key {
            popup(None);
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    card.add_controller(keys);
}
