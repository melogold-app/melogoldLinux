//! Ссылки (задание 0010): «Скопировать ссылку», свой плейлист через `POST /shares`, открытие ссылки
//! Melogold (`melogold://share`, `/s/<код>`) и ссылок других сервисов. Разбор ссылок — `melogold-core`,
//! сеть — `melogold-server` и `melogold-innertube::external`; здесь только окно.

use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use melogold_core::music::Track;
use melogold_core::music_services::{self, MusicLinkKind, MusicServiceLink};
use melogold_core::server_address;
use melogold_core::share_links;
use melogold_core::youtube_links;
use melogold_innertube::external::Resolution;
use melogold_playback::engine::Command;
use melogold_server::account::Account;
use melogold_server::dto::TrackInput;

use crate::catalog_widgets::CollectionHeader;
use crate::localization::tr;
use crate::window::MainWindow;

/// Самый длинный снимок плейлиста (API §4.11).
const SHARE_MAX_TRACKS: usize = 1000;

/// Кнопка «Скопировать ссылку» в шапке альбома, исполнителя, плейлиста.
pub fn add_copy_link(window: &MainWindow, header: &CollectionHeader, url: String) {
    let weak = window.downgrade();
    header.add_button(tr("LinuxCopyLink"), "edit-copy-symbolic", false, move || {
        if let Some(window) = weak.upgrade() {
            window.copy_link_text(&url, None);
        }
    });
}

/// Подпись первой строки подсказки поиска для ссылок этого модуля: «Открыть ссылку: плейлист».
pub fn suggestion_kind(text: &str) -> Option<&'static str> {
    if share_links::parse_share_url(text).is_some() {
        return Some("LinkKindPlaylist");
    }
    music_services::parse(text).map(|link| match link.kind {
        MusicLinkKind::Album => "LinkKindAlbum",
        MusicLinkKind::Artist => "LinuxLinkKindArtist",
        MusicLinkKind::Playlist => "LinkKindPlaylist",
        MusicLinkKind::Track | MusicLinkKind::Unknown => "LinuxLinkKindTrack",
    })
}

/// Что вышло из «Скопировать ссылку» у своего плейлиста.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shared {
    /// Снимок на сервере: `…/s/<код>`.
    Link(String),
    /// Запасная ссылка `watch_videos`; `truncated` — треков больше, чем в неё войдёт.
    Fallback { url: String, truncated: bool },
    /// Сервер отказал: снимков уже слишком много.
    Limit,
}

/// Снимок своего плейлиста при `features.share` и входе; иначе (и при любой ошибке сервера, кроме лимита)
/// — ссылка YouTube на первые 50 треков.
async fn share_playlist(account: Arc<Account>, name: String, tracks: Vec<Track>) -> Shared {
    let can_share = account.session().is_some() && account.ensure_server_info().await.is_some_and(|info| info.features.share.is_some());
    if can_share {
        let inputs: Vec<TrackInput> = tracks.iter().take(SHARE_MAX_TRACKS).map(TrackInput::from_track).collect();
        match account.create_share(&name, inputs).await {
            Ok(created) => return Shared::Link(created.url),
            Err(error) if error.code == "share_limit_reached" => return Shared::Limit,
            Err(error) => tracing::warn!(%error, "снимок плейлиста не создан, ссылка YouTube"),
        }
    }
    let (url, truncated) = share_links::watch_videos_url(tracks.iter().map(|t| t.video_id.as_str()));
    Shared::Fallback { url, truncated }
}

impl MainWindow {
    /// Ссылка в буфер обмена и «Ссылка скопирована» (при `note` — с пояснением).
    pub fn copy_link_text(&self, url: &str, note: Option<&str>) {
        self.window.clipboard().set_text(url);
        match note {
            Some(note) => self.toast(&format!("{} · {note}", tr("LinkCopied"))),
            None => self.toast(tr("LinkCopied")),
        }
    }

    /// «Скопировать ссылку» у своего плейлиста.
    pub fn share_local_playlist(&self, id: i64) {
        let load = self.ctx.services.db(move |library| {
            let playlist = library.playlist(id).ok().flatten()?;
            let tracks: Vec<Track> = library.playlist_tracks(id).unwrap_or_default().iter().map(|t| library.display(t)).collect();
            Some((playlist.name, tracks))
        });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(window) = weak.upgrade() else { return };
            let Some(Some((name, tracks))) = load.await else { return };
            if tracks.is_empty() {
                window.toast(tr("LinuxShareEmpty"));
                return;
            }
            let account = Arc::clone(&window.ctx.services.account);
            let Some(shared) = window.ctx.services.run(share_playlist(account, name, tracks)).await else { return };
            match shared {
                Shared::Link(url) => window.copy_link_text(&url, None),
                Shared::Fallback { url, truncated } => window.copy_link_text(&url, truncated.then(|| tr("LinuxShareFirst50"))),
                Shared::Limit => window.toast(tr("LinuxShareLimit")),
            }
        });
    }

    /// Ссылка из текста, которую этот модуль понимает: снимок плейлиста (`/s/<код>`) или ссылка другого
    /// сервиса. `true` — ссылка принята и открывается.
    pub fn open_incoming(&self, text: &str) -> bool {
        if let Some(share) = share_links::parse_share_url(text) {
            self.open_share_link(&share.base, &share.id);
            return true;
        }
        if let Some(link) = music_services::parse(text) {
            self.open_service_link(link);
            return true;
        }
        false
    }

    /// «Плейлист по ссылке»: сервер из ссылки проверяется по §7.1 (адрес, транспорт); входа не нужно.
    pub fn open_share_link(&self, server: &str, id: &str) {
        match server_address::normalize(server) {
            Ok(address) => self.push(&crate::pages::shares::shared_playlist_page(self, &address.url, id)),
            Err(_) => self.toast(tr("LinkUnsupported")),
        }
    }

    /// Spotify, Apple Music, Яндекс, Deezer, Tidal, SoundCloud: то же самое в YouTube Music.
    fn open_service_link(&self, link: MusicServiceLink) {
        if link.kind == MusicLinkKind::Playlist {
            self.toast(tr("LinuxImportPlaylistsLater"));
            return;
        }
        let progress = adw::Toast::builder().title(tr("LinuxLinkOpening")).timeout(0).build();
        self.add_toast(progress.clone());
        let resolver = Arc::clone(&self.ctx.services.external);
        let task = self.ctx.services.run(async move { resolver.resolve(&link).await });
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let result = task.await;
            progress.dismiss();
            let Some(window) = weak.upgrade() else { return };
            tracing::info!(результат = ?result.as_ref().map(std::mem::discriminant), "ссылка другого сервиса");
            match result {
                Some(Resolution::Track(track)) => {
                    window.ctx.services.player.send(Command::PlaySingle { track: *track, start: std::time::Duration::ZERO })
                }
                Some(Resolution::Album(album)) => window.push(&crate::pages::catalog::album_page(&window, &album.browse_id)),
                Some(Resolution::Artist(artist)) => window.push(&crate::pages::catalog::artist_page(&window, &artist.browse_id)),
                Some(Resolution::YouTube(url)) => window.open_youtube(youtube_links::parse(&url)),
                Some(Resolution::NotFound) | None => window.toast(tr("LinkUnsupported")),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_words_by_kind() {
        assert_eq!(suggestion_kind("https://open.spotify.com/track/4PTG3Z6ehGkBFwjybzWkR8"), Some("LinuxLinkKindTrack"));
        assert_eq!(suggestion_kind("https://music.apple.com/us/album/x/1"), Some("LinkKindAlbum"));
        assert_eq!(suggestion_kind("https://music.yandex.ru/artist/79215"), Some("LinuxLinkKindArtist"));
        assert_eq!(suggestion_kind("https://music.example.com/s/a1B2c3D4e5"), Some("LinkKindPlaylist"));
        assert_eq!(suggestion_kind("https://music.youtube.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(suggestion_kind("Кино"), None);
    }
}
