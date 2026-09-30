//! MPRIS2 (`org.mpris.MediaPlayer2.melogold`, docs/PROMPT.md §4 «Рабочий стол»): медиаклавиши,
//! плеер в GNOME Shell и KDE. Своих глобальных клавиш на Wayland нет — только это.
//!
//! Живёт в рантайме tokio: команды идут прямо в плеер, а то, что касается окна и настроек
//! (показать окно, выйти, громкость, скорость, перемешивание, повтор), — в главный поток.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use melogold_core::app_info::APP_ID;
use melogold_core::queue::RepeatMode;
use melogold_core::thumbnails;
use melogold_data::{Change, Library};
use melogold_playback::engine::{Command, Event, PlayerHandle, State, Status};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PATH: &str = "/org/mpris/MediaPlayer2";
const NO_TRACK: &str = "/org/mpris/MediaPlayer2/TrackList/NoTrack";

/// Что MPRIS просит у главного потока.
#[derive(Debug)]
pub enum Request {
    Raise,
    Quit,
    Volume(f64),
    Rate(f64),
    Shuffle(bool),
    Repeat(RepeatMode),
}

struct Root {
    ui: async_channel::Sender<Request>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        let _ = self.ui.try_send(Request::Raise);
    }

    fn quit(&self) {
        let _ = self.ui.try_send(Request::Quit);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn identity(&self) -> &str {
        "Melogold"
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> &str {
        APP_ID
    }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    player: PlayerHandle,
    ui: async_channel::Sender<Request>,
    state: State,
    /// Свои названия треков (задание 0005): рабочий стол показывает то же, что окно.
    library: Arc<Library>,
    /// Обложка для рабочего стола, уже квадратом без полей и обводки: адрес картинки → `file://`.
    art: Option<(String, String)>,
}

/// Готовые квадратные обложки из окна: адрес картинки и `file://` её квадрата.
static ART: OnceLock<async_channel::Sender<(String, String)>> = OnceLock::new();

/// Обложка для рабочего стола (задание 0014): картинку «Сейчас играет» — уже без полей и обводки —
/// окно режет по центру в квадрат и кладёт PNG в кэш; оболочка берёт `file://`. Пока файла нет, в
/// `mpris:artUrl` — адрес картинки, как раньше.
pub fn publish_art(cache: &std::path::Path, url: &str, texture: &gtk::gdk::Texture) {
    use gtk::prelude::*;
    let Some(sender) = ART.get().cloned() else { return };
    let (width, height) = (texture.width().max(0) as usize, texture.height().max(0) as usize);
    let side = width.min(height);
    if side == 0 {
        return;
    }
    let mut downloader = gtk::gdk::TextureDownloader::new(texture);
    downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let dir = cache.join("mpris");
    let (url, name) = (url.to_owned(), format!("{:016x}.png", fnv(url)));
    gtk::glib::spawn_future_local(async move {
        let file = gtk::gio::spawn_blocking(move || -> Option<PathBuf> {
            std::fs::create_dir_all(&dir).ok()?;
            // Одна картинка на трек; прежние не копятся.
            for entry in std::fs::read_dir(&dir).ok()?.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
            let path = dir.join(name);
            square_png(&bytes, width, height, stride, &path)?;
            Some(path)
        })
        .await
        .ok()
        .flatten();
        if let Some(path) = file {
            let _ = sender.try_send((url, format!("file://{}", path.display())));
        }
    });
}

/// Центральный квадрат картинки RGBA (`stride` — байт в строке) в PNG.
fn square_png(rgba: &[u8], width: usize, height: usize, stride: usize, path: &std::path::Path) -> Option<()> {
    use gtk::prelude::*;
    let side = width.min(height);
    let (x0, y0) = ((width - side) / 2, (height - side) / 2);
    let mut square = Vec::with_capacity(side * side * 4);
    for y in y0..y0 + side {
        let start = y * stride + x0 * 4;
        square.extend_from_slice(rgba.get(start..start + side * 4)?);
    }
    let texture = gtk::gdk::MemoryTexture::new(
        side as i32,
        side as i32,
        gtk::gdk::MemoryFormat::R8g8b8a8,
        &gtk::glib::Bytes::from_owned(square),
        side * 4,
    );
    texture.save_to_png(path).ok()
}

/// Имя файла по адресу картинки (FNV-1a): новый трек — новое имя, и оболочка перечитывает файл.
fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

/// Путь трека для MPRIS: из id элемента очереди — он у каждого элемента свой.
fn track_path(state: &State) -> ObjectPath<'static> {
    match state.item_id {
        Some(id) if state.track.is_some() => {
            ObjectPath::try_from(format!("/app/melogold/Melogold/track/{}", id.max(0))).unwrap_or_else(|_| no_track())
        }
        _ => no_track(),
    }
}

fn no_track() -> ObjectPath<'static> {
    ObjectPath::from_static_str_unchecked(NO_TRACK)
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.player.send(Command::Next);
    }

    fn previous(&self) {
        self.player.send(Command::Previous);
    }

    fn pause(&self) {
        self.player.send(Command::Pause);
    }

    fn play_pause(&self) {
        self.player.send(Command::TogglePlay);
    }

    fn stop(&self) {
        self.player.send(Command::Pause);
    }

    fn play(&self) {
        self.player.send(Command::Play);
    }

    /// Сдвиг в микросекундах.
    fn seek(&self, offset: i64) {
        self.player.send(Command::SeekBy(offset / 1000));
    }

    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        if track_id == track_path(&self.state) && position >= 0 {
            self.player.send(Command::Seek(Duration::from_micros(position as u64)));
        }
    }

    fn open_uri(&self, _uri: &str) {}

    #[zbus(property)]
    fn playback_status(&self) -> &str {
        match (self.state.track.is_some(), self.state.playing, self.state.status) {
            (false, ..) | (_, _, Status::Idle) => "Stopped",
            (true, true, _) => "Playing",
            _ => "Paused",
        }
    }

    #[zbus(property)]
    fn loop_status(&self) -> &str {
        match self.state.repeat {
            RepeatMode::Off => "None",
            RepeatMode::All => "Playlist",
            RepeatMode::One => "Track",
        }
    }

    #[zbus(property)]
    fn set_loop_status(&mut self, value: String) {
        let mode = match value.as_str() {
            "Track" => RepeatMode::One,
            "Playlist" => RepeatMode::All,
            _ => RepeatMode::Off,
        };
        let _ = self.ui.try_send(Request::Repeat(mode));
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        self.state.speed
    }

    #[zbus(property)]
    fn set_rate(&mut self, value: f64) {
        if value > 0.0 {
            let _ = self.ui.try_send(Request::Rate(value.clamp(0.5, 2.0)));
        }
    }

    #[zbus(property)]
    fn shuffle(&self) -> bool {
        self.state.shuffle
    }

    #[zbus(property)]
    fn set_shuffle(&mut self, value: bool) {
        let _ = self.ui.try_send(Request::Shuffle(value));
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let mut put = |key: &str, value: Value<'_>| {
            if let Ok(owned) = value.try_to_owned() {
                map.insert(key.to_owned(), owned);
            }
        };
        put("mpris:trackid", Value::from(track_path(&self.state)));
        let Some(track) = self.state.track.as_ref().map(|t| self.library.display(t)) else { return map };
        put("xesam:title", Value::from(track.title.clone()));
        let artists: Vec<String> = match track.artists_text.clone() {
            Some(text) if !text.is_empty() => vec![text],
            _ => track.artists.iter().map(|a| a.name.clone()).collect(),
        };
        put("xesam:artist", Value::from(artists));
        if let Some(album) = &track.album_title {
            put("xesam:album", Value::from(album.clone()));
        }
        if let Some(duration) = self.state.duration.or(track.duration_ms.map(|ms| Duration::from_millis(ms as u64))) {
            put("mpris:length", Value::from(duration.as_micros() as i64));
        }
        // Обложка квадратом без полей и обводки — файл из окна (задание 0014); пока его нет — адрес.
        let art = track.thumbnail_url.clone().unwrap_or_else(|| thumbnails::for_video(&track.video_id, 544));
        if let Some(url) = thumbnails::sized(Some(&art), 544) {
            let file = self.art.as_ref().filter(|(source, _)| *source == url).map(|(_, file)| file.clone());
            put("mpris:artUrl", Value::from(file.unwrap_or(url)));
        }
        put("xesam:url", Value::from(format!("https://music.youtube.com/watch?v={}", track.video_id)));
        map
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        if self.state.muted {
            0.0
        } else {
            self.state.volume
        }
    }

    #[zbus(property)]
    fn set_volume(&mut self, value: f64) {
        let _ = self.ui.try_send(Request::Volume(value.clamp(0.0, 1.0)));
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.player.position().map(|p| p.as_micros() as i64).unwrap_or(0)
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        0.5
    }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        2.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.state.has_next
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.state.has_previous
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.state.track.is_some()
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.state.track.is_some()
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.state.track.as_ref().is_some_and(|t| !t.is_live())
    }

    #[zbus(property(emits_changed_signal = "const"))]
    fn can_control(&self) -> bool {
        true
    }

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;
}

/// Поднять MPRIS; события плеера обновляют свойства.
pub fn start(runtime: &tokio::runtime::Handle, player: PlayerHandle, library: Arc<Library>, ui: async_channel::Sender<Request>) {
    let events = player.subscribe();
    runtime.spawn(async move {
        if let Err(error) = serve(player, library, ui, events).await {
            tracing::warn!(%error, "MPRIS не поднялся: медиаклавиши и плеер рабочего стола не будут работать");
        }
    });
}

async fn serve(
    player: PlayerHandle,
    library: Arc<Library>,
    ui: async_channel::Sender<Request>,
    events: async_channel::Receiver<Event>,
) -> zbus::Result<()> {
    let state = player.state();
    // Правка названия играющего трека — новые метаданные.
    let (edited, edits) = async_channel::unbounded();
    let (art_sender, arts) = async_channel::bounded::<(String, String)>(4);
    let _ = ART.set(art_sender);
    library.subscribe(move |change| {
        if change.has(Change::OVERRIDES) {
            let _ = edited.try_send(());
        }
    });
    let connection = zbus::connection::Builder::session()?
        .name("org.mpris.MediaPlayer2.melogold")?
        .serve_at(PATH, Root { ui: ui.clone() })?
        .serve_at(PATH, Player { player, ui, state, library, art: None })?
        .build()
        .await?;
    tracing::info!("MPRIS: org.mpris.MediaPlayer2.melogold");
    let iface = connection.object_server().interface::<_, Player>(PATH).await?;
    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Ok(event) => event,
                Err(_) => break,
            },
            Ok(()) = edits.recv() => {
                iface.get().await.metadata_changed(iface.signal_emitter()).await?;
                continue;
            }
            Ok(art) = arts.recv() => {
                let mut player = iface.get_mut().await;
                player.art = Some(art);
                player.metadata_changed(iface.signal_emitter()).await?;
                continue;
            }
        };
        let emitter = iface.signal_emitter();
        match event {
            Event::State(state) => {
                let mut player = iface.get_mut().await;
                let old = std::mem::replace(&mut player.state, *state);
                let new = &player.state;
                if old.track != new.track || old.item_id != new.item_id || old.duration != new.duration {
                    player.metadata_changed(emitter).await?;
                    player.can_seek_changed(emitter).await?;
                    player.can_play_changed(emitter).await?;
                    player.can_pause_changed(emitter).await?;
                }
                if old.playing != new.playing || old.status != new.status || old.track.is_some() != new.track.is_some() {
                    player.playback_status_changed(emitter).await?;
                }
                if old.repeat != new.repeat {
                    player.loop_status_changed(emitter).await?;
                }
                if old.shuffle != new.shuffle {
                    player.shuffle_changed(emitter).await?;
                }
                if old.speed != new.speed {
                    player.rate_changed(emitter).await?;
                }
                if old.volume != new.volume || old.muted != new.muted {
                    player.volume_changed(emitter).await?;
                }
                if old.has_next != new.has_next {
                    player.can_go_next_changed(emitter).await?;
                }
                if old.has_previous != new.has_previous {
                    player.can_go_previous_changed(emitter).await?;
                }
            }
            Event::Seeked(position) => Player::seeked(emitter, position.as_micros() as i64).await?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кадр 16:9 с красной серединой и синими краями → квадрат по центру: краёв в нём нет.
    #[test]
    fn desktop_art_is_the_centre_square() {
        use gtk::prelude::*;
        let (width, height) = (16usize, 9usize);
        let mut rgba = Vec::with_capacity(width * height * 4);
        for _ in 0..height {
            for x in 0..width {
                let centre = (4..12).contains(&x);
                rgba.extend_from_slice(if centre { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
            }
        }
        let dir = std::env::temp_dir().join(format!("melogold-mpris-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("папка");
        let path = dir.join("art.png");
        square_png(&rgba, width, height, width * 4, &path).expect("PNG записан");
        let texture = gtk::gdk::Texture::from_filename(&path).expect("PNG читается");
        assert_eq!((texture.width(), texture.height()), (9, 9));
        let mut pixels = vec![0u8; 9 * 9 * 4];
        texture.download(&mut pixels, 9 * 4);
        // Выгрузка — B, G, R, A. Квадрат — столбцы 3..12 кадра: первый из них синий, дальше красные.
        assert_eq!(&pixels[0..3], &[255, 0, 0], "столбец 3 кадра — синий");
        assert_eq!(&pixels[4..7], &[0, 0, 255], "столбец 4 кадра — красный");
        assert_eq!(&pixels[8 * 4..8 * 4 + 3], &[0, 0, 255], "столбец 11 кадра — красный");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn art_file_name_follows_the_picture() {
        assert_ne!(fnv("https://i.ytimg.com/vi/a/hq720.jpg"), fnv("https://i.ytimg.com/vi/b/hq720.jpg"));
        assert_eq!(fnv("x"), fnv("x"));
    }
}
