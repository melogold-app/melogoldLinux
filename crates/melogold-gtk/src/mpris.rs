//! MPRIS2 (`org.mpris.MediaPlayer2.melogold`, docs/PROMPT.md §4 «Рабочий стол»): медиаклавиши,
//! плеер в GNOME Shell и KDE. Своих глобальных клавиш на Wayland нет — только это.
//!
//! Живёт в рантайме tokio: команды идут прямо в плеер, а то, что касается окна и настроек
//! (показать окно, выйти, громкость, скорость, перемешивание, повтор), — в главный поток.

use std::collections::HashMap;
use std::time::Duration;

use melogold_core::app_info::APP_ID;
use melogold_core::queue::RepeatMode;
use melogold_core::thumbnails;
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
        let Some(track) = &self.state.track else { return map };
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
        // Квадратная обложка: у видео — середина кадра делается оболочкой, адрес — наш.
        let art = track.thumbnail_url.clone().unwrap_or_else(|| thumbnails::for_video(&track.video_id, 544));
        if let Some(url) = thumbnails::sized(Some(&art), 544) {
            put("mpris:artUrl", Value::from(url));
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
pub fn start(runtime: &tokio::runtime::Handle, player: PlayerHandle, ui: async_channel::Sender<Request>) {
    let events = player.subscribe();
    runtime.spawn(async move {
        if let Err(error) = serve(player, ui, events).await {
            tracing::warn!(%error, "MPRIS не поднялся: медиаклавиши и плеер рабочего стола не будут работать");
        }
    });
}

async fn serve(player: PlayerHandle, ui: async_channel::Sender<Request>, events: async_channel::Receiver<Event>) -> zbus::Result<()> {
    let state = player.state();
    let connection = zbus::connection::Builder::session()?
        .name("org.mpris.MediaPlayer2.melogold")?
        .serve_at(PATH, Root { ui: ui.clone() })?
        .serve_at(PATH, Player { player, ui, state })?
        .build()
        .await?;
    tracing::info!("MPRIS: org.mpris.MediaPlayer2.melogold");
    let iface = connection.object_server().interface::<_, Player>(PATH).await?;
    while let Ok(event) = events.recv().await {
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
