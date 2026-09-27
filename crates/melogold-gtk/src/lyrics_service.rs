//! Текст играющего трека (Windows `LyricsService.cs`, Android `PlayerLyricsState`): из кэша
//! библиотеки, недостающее — из сети, пока текст на экране. Порядок выбора (задание 0006): свой →
//! закреплённый → поиск (YouTube Music → LrcLib → KuGou, у видео LrcLib первым) → общий с сервера.
//! Неудача из-за сети не кэшируется как «текста нет». Здесь же — синхронный или обычный, сдвиг,
//! выбор в LrcLib, файл и сохранение из редактора.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk::glib;
use melogold_core::lyrics::draft::LyricsDraft;
use melogold_core::lyrics::pins;
use melogold_core::lyrics::rows::{self, LyricRow};
use melogold_core::lyrics::sync_rules::{sources, StoredLyrics};
use melogold_core::lyrics::{detect, parse_synced, ttml, Format, SyncedLyrics};
use melogold_core::music::Track;
use melogold_core::settings::keys;
use melogold_innertube::lyrics::{FetchResult, LrcLibTrack};

use crate::app::AppContext;

/// Что показывает область текста (Android `LyricsContent`).
#[derive(Clone, Debug, Default)]
pub enum LyricsState {
    /// Трека нет или текст не на экране и в кэше ничего.
    #[default]
    Unknown,
    Loading,
    /// `offset_ms` — сдвиг у этого трека: положительный — текст раньше.
    Synced {
        lyrics: Rc<SyncedLyrics>,
        rows: Rc<Vec<LyricRow>>,
        offset_ms: i64,
        source: Option<String>,
    },
    Plain {
        text: String,
        source: Option<String>,
    },
    /// Поставщики ответили, текста нет.
    NotFound,
    /// Не удалось из-за сети; в кэш ничего не записано.
    Failed,
}

#[derive(Clone)]
pub struct LyricsService(Rc<Inner>);

pub struct Inner {
    ctx: Rc<AppContext>,
    state: RefCell<LyricsState>,
    /// Трек, каким его дал YouTube (ключ в базе и поставщики).
    track: RefCell<Option<Track>>,
    /// Свои название и исполнитель трека (задание 0005): LrcLib спрашивается сначала по ним.
    edited: RefCell<Option<(String, String)>>,
    stored: RefCell<Option<StoredLyrics>>,
    active: Cell<bool>,
    generation: Cell<u64>,
    /// Последний разобранный синхронный текст: тот же текст — те же строки, экран не начинает заново.
    parsed: RefCell<Option<(String, Option<(Rc<SyncedLyrics>, Rc<Vec<LyricRow>>)>)>>,
    listeners: RefCell<Vec<Weak<dyn Fn()>>>,
}

impl std::ops::Deref for LyricsService {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl LyricsService {
    pub fn new(ctx: Rc<AppContext>) -> LyricsService {
        LyricsService(Rc::new(Inner {
            ctx,
            state: RefCell::default(),
            track: RefCell::default(),
            edited: RefCell::default(),
            stored: RefCell::default(),
            active: Cell::new(false),
            generation: Cell::new(0),
            parsed: RefCell::default(),
            listeners: RefCell::default(),
        }))
    }

    /// Экран, который перерисовывается при смене состояния; живёт, пока жив `refresh`.
    pub fn listen(&self, refresh: &Rc<dyn Fn()>) {
        self.listeners.borrow_mut().push(Rc::downgrade(refresh));
    }

    fn notify(&self) {
        let alive: Vec<Rc<dyn Fn()>> = {
            let mut list = self.listeners.borrow_mut();
            list.retain(|l| l.strong_count() > 0);
            list.iter().filter_map(Weak::upgrade).collect()
        };
        for refresh in alive {
            refresh();
        }
    }

    pub fn state(&self) -> LyricsState {
        self.state.borrow().clone()
    }

    pub fn track(&self) -> Option<Track> {
        self.track.borrow().clone()
    }

    pub fn stored(&self) -> Option<StoredLyrics> {
        self.stored.borrow().clone()
    }

    /// Синхронный текст есть — переключателю есть на что переключаться.
    pub fn has_synced(&self) -> bool {
        self.stored.borrow().as_ref().and_then(|s| s.synced.as_deref()).is_some_and(|t| !t.is_empty() && parse_synced(t).is_some())
    }

    /// Играет другой трек (или свои названия изменились): текст — заново.
    pub fn set_track(&self, track: Option<&Track>, display: Option<&Track>) {
        let edited = match (track, display) {
            (Some(track), Some(display)) if display.title != track.title || display.artists_text != track.artists_text => {
                Some((display.artists_text.clone().unwrap_or_default(), display.title.clone()))
            }
            _ => None,
        };
        self.edited.replace(edited);
        let same = self.track.borrow().as_ref().map(|t| &t.video_id) == track.map(|t| &t.video_id);
        if same {
            return;
        }
        self.track.replace(track.cloned());
        self.reload();
    }

    /// Текст на экране: только тогда недостающее ищется в сети.
    pub fn set_active(&self, active: bool) {
        if self.active.replace(active) != active && active {
            self.reload();
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.get()
    }

    pub fn retry(&self) {
        self.reload();
    }

    pub fn reload(&self) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let Some(track) = self.track() else {
            self.stored.replace(None);
            self.set(LyricsState::Unknown);
            return;
        };
        let this = self.clone();
        glib::spawn_future_local(async move { this.load(track, generation).await });
    }

    /// Библиотека изменилась (синхронизация принесла свою версию или закрепление): перечитать, если
    /// это касается играющего трека. Своя же запись сюда возвращается тем же текстом и ничего не меняет.
    pub fn library_changed(&self) {
        let Some(video_id) = self.track().map(|t| t.video_id) else { return };
        let task =
            self.ctx.services.db(move |library| (library.lyrics(&video_id).ok().flatten(), library.lyrics_pin(&video_id).ok().flatten()));
        let this = self.clone();
        let generation = self.generation.get();
        glib::spawn_future_local(async move {
            let Some((stored, pin)) = task.await else { return };
            if this.generation.get() != generation {
                return;
            }
            let differs = *this.stored.borrow() != stored;
            if differs || (this.active.get() && pins::needs_pinned(stored.as_ref(), pin.as_ref())) {
                this.reload();
            }
        });
    }

    async fn load(&self, track: Track, generation: u64) {
        let video_id = track.video_id.clone();
        let loaded = self
            .ctx
            .services
            .db(move |library| (library.lyrics(&video_id).ok().flatten(), library.lyrics_pin(&video_id).ok().flatten()))
            .await;
        let Some((stored, pin)) = loaded else { return };
        if self.generation.get() != generation {
            return;
        }
        self.stored.replace(stored.clone());
        let needs_pin = pins::needs_pinned(stored.as_ref(), pin.as_ref());
        let missing = stored.as_ref().is_none_or(|s| s.plain.is_none() || s.synced.is_none());
        if !self.active.get() || !(needs_pin || missing) {
            self.set(self.content(stored.as_ref()));
            return;
        }
        // Что уже есть — сразу на экран; недостающее ищется, не пряча текст за «Загрузкой».
        let known = self.content(stored.as_ref());
        self.set(if matches!(known, LyricsState::Synced { .. } | LyricsState::Plain { .. }) { known } else { LyricsState::Loading });

        let fetcher = Arc::clone(&self.ctx.services.lyrics);
        let duration = self.ctx.services.player.state().duration.map(|d| d.as_millis() as i64).filter(|d| *d > 0);
        let duration = duration.or(track.duration_ms).unwrap_or(0);
        let edited = self.edited.borrow().clone();
        let current = stored.clone();
        let task = self.ctx.services.run(async move {
            let mut current = current;
            if let Some(pin) = pin.filter(|_| needs_pin) {
                // Поставщик молчит или текста по ссылке нет — обычный поиск; закрепление не снимается.
                match fetcher.pinned(&pin.source, &pin.reference).await {
                    Ok(Some(lyrics)) => current = Some(pins::from_pinned(lyrics, &pin, current.as_ref())),
                    Ok(None) => {}
                    Err(error) => tracing::info!(?error, "закреплённый текст не пришёл"),
                }
            }
            match current {
                Some(lyrics) if lyrics.plain.is_some() && lyrics.synced.is_some() => {
                    FetchResult { lyrics, any_failure: false, mine: false }
                }
                current => fetcher.fetch(&track, edited, duration, current.as_ref()).await,
            }
        });
        let Some(result) = task.await else { return };
        if self.generation.get() != generation {
            return;
        }
        let video_id = self.track().map(|t| t.video_id).unwrap_or_default();
        let mut fetched = result.lyrics;
        fetched.chosen = fetched.chosen || stored.as_ref().is_some_and(|s| s.chosen) || result.mine;
        // Недостающая сторона, которую не удалось получить из-за сети, остаётся «ещё не искали» (None),
        // а найденная сохраняется — иначе при каждом открытии всё ищется заново.
        if result.any_failure && (fetched.plain.is_none() || fetched.synced.is_none()) {
            self.stored.replace(Some(fetched.clone()));
            if fetched.plain.is_some() || fetched.synced.is_some() {
                self.save(&video_id, fetched.clone());
            }
            let partial = self.content(Some(&fetched));
            self.set(if matches!(partial, LyricsState::Synced { .. } | LyricsState::Plain { .. }) { partial } else { LyricsState::Failed });
            return;
        }
        fetched.synced.get_or_insert_with(String::new);
        fetched.plain.get_or_insert_with(String::new);
        self.stored.replace(Some(fetched.clone()));
        self.save(&video_id, fetched.clone());
        self.set(self.content(Some(&fetched)));
    }

    fn save(&self, video_id: &str, lyrics: StoredLyrics) {
        let video_id = video_id.to_owned();
        let task = self.ctx.services.db(move |library| library.save_lyrics(&video_id, &lyrics));
        glib::spawn_future_local(async move {
            if let Some(Err(error)) = task.await {
                tracing::warn!(%error, "текст не сохранился");
            }
        });
    }

    fn parsed(&self, text: &str) -> Option<(Rc<SyncedLyrics>, Rc<Vec<LyricRow>>)> {
        if let Some((cached, parsed)) = self.parsed.borrow().as_ref() {
            if cached == text {
                return parsed.clone();
            }
        }
        let parsed = parse_synced(text).map(|lyrics| {
            let rows = rows::build(&lyrics);
            (Rc::new(lyrics), Rc::new(rows))
        });
        self.parsed.replace(Some((text.to_owned(), parsed.clone())));
        parsed
    }

    fn content(&self, stored: Option<&StoredLyrics>) -> LyricsState {
        let synced = stored.and_then(|s| s.synced.as_deref()).filter(|t| !t.is_empty()).and_then(|t| self.parsed(t));
        let synced = synced.filter(|(_, rows)| !rows.is_empty());
        let prefer_synced = self.ctx.settings.get(&keys::LYRICS_VIEW) != "plain";
        if let (true, Some((lyrics, rows)), Some(stored)) = (prefer_synced, &synced, stored) {
            return LyricsState::Synced {
                lyrics: Rc::clone(lyrics),
                rows: Rc::clone(rows),
                offset_ms: stored.offset_ms,
                source: stored.synced_source.clone(),
            };
        }
        if let Some(plain) = stored.and_then(|s| s.plain.as_deref()).filter(|p| !p.is_empty()) {
            return LyricsState::Plain { text: plain.to_owned(), source: stored.and_then(|s| s.plain_source.clone()) };
        }
        if let (Some((lyrics, _)), Some(stored)) = (&synced, stored) {
            return LyricsState::Plain { text: lyrics.plain_text(), source: stored.synced_source.clone() };
        }
        if stored.is_some_and(|s| s.plain.is_some() && s.synced.is_some()) {
            return LyricsState::NotFound;
        }
        if self.active.get() {
            LyricsState::Loading
        } else {
            LyricsState::Unknown
        }
    }

    fn set(&self, state: LyricsState) {
        self.state.replace(state);
        self.notify();
    }

    fn update(&self, lyrics: StoredLyrics) {
        let Some(track) = self.track() else { return };
        self.stored.replace(Some(lyrics.clone()));
        self.save(&track.video_id, lyrics.clone());
        self.set(self.content(Some(&lyrics)));
    }

    /// Синхронный ↔ обычный: подпись переключателя — по тому, что на экране.
    pub fn toggle_synced(&self) {
        let synced = matches!(*self.state.borrow(), LyricsState::Synced { .. });
        self.ctx.settings.set(&keys::LYRICS_VIEW, if synced { "plain".to_owned() } else { "synced".to_owned() });
        self.set(self.content(self.stored.borrow().clone().as_ref()));
    }

    /// Сдвиг синхронного текста этого трека: положительный — раньше. Закреплённый текст забирает
    /// сдвиг «позже» в закрепление (задание 0006).
    pub fn shift(&self, delta_ms: i64) {
        let Some(stored) = self.stored() else { return };
        self.set_offset(StoredLyrics { offset_ms: stored.offset_ms + delta_ms, ..stored });
    }

    pub fn reset_shift(&self) {
        let Some(stored) = self.stored() else { return };
        self.set_offset(StoredLyrics { offset_ms: 0, ..stored });
    }

    fn set_offset(&self, lyrics: StoredLyrics) {
        let Some(track) = self.track() else { return };
        self.stored.replace(Some(lyrics.clone()));
        self.set(self.content(Some(&lyrics)));
        // Текст и закрепление — одной работой: перечитывание по изменению библиотеки видит их вместе.
        let video_id = track.video_id;
        let task = self.ctx.services.db(move |library| {
            library.save_lyrics(&video_id, &lyrics)?;
            library.shift_pin(&video_id, &lyrics)
        });
        glib::spawn_future_local(async move {
            if let Some(Err(error)) = task.await {
                tracing::warn!(%error, "сдвиг текста не сохранился");
            }
        });
    }

    /// Текст, выбранный в «Найти другой текст»: свой (задание 0002) — через 2 с уходит на сервер, и
    /// на других устройствах искать его заново не нужно.
    pub fn use_lrclib(&self, found: &LrcLibTrack) {
        let current = self.stored().unwrap_or_default();
        let synced = found.synced_lyrics.clone().filter(|t| !t.trim().is_empty());
        let plain = found.plain_lyrics.clone().filter(|t| !t.trim().is_empty());
        let reference = Some(found.id.to_string());
        let has_synced = synced.is_some();
        let lyrics = StoredLyrics {
            synced_source: if has_synced { Some(sources::LRCLIB.into()) } else { current.synced_source.clone() },
            synced_ref: if has_synced { reference.clone() } else { current.synced_ref.clone() },
            offset_ms: if has_synced { 0 } else { current.offset_ms },
            synced: synced.or(current.synced.clone()),
            plain_source: if plain.is_some() { Some(sources::LRCLIB.into()) } else { current.plain_source.clone() },
            plain_ref: if plain.is_some() { reference } else { current.plain_ref.clone() },
            plain: plain.or(current.plain.clone()),
            language: current.language.clone(),
            chosen: true,
        };
        self.ctx.settings.set(&keys::LYRICS_VIEW, if has_synced { "synced".to_owned() } else { "plain".to_owned() });
        self.update(lyrics);
    }

    /// Текст из файла: TTML или LRC — синхронный, иначе обычный. `false` — в файле нет текста.
    pub fn import(&self, text: &str) -> bool {
        if text.trim().is_empty() || self.track().is_none() {
            return false;
        }
        let current = self.stored().unwrap_or_default();
        if let Some(parsed) = parse_synced(text) {
            self.ctx.settings.set(&keys::LYRICS_VIEW, "synced".to_owned());
            self.update(StoredLyrics {
                synced: Some(text.to_owned()),
                synced_source: Some(sources::FILE.into()),
                synced_ref: None,
                offset_ms: 0,
                language: parsed.language.or(current.language.clone()),
                ..current
            });
            true
        } else if detect(text) == Format::Plain {
            self.update(StoredLyrics {
                plain: Some(text.trim().to_owned()),
                plain_source: Some(sources::FILE.into()),
                plain_ref: None,
                ..current
            });
            true
        } else {
            false
        }
    }

    /// Черновик для редактора (Android `initialDraft`): синхронный текст во времени трека, иначе
    /// обычный, иначе пусто.
    pub fn initial_draft(&self) -> LyricsDraft {
        let stored = self.stored();
        if let Some(stored) = &stored {
            if let Some(synced) = stored.synced.as_deref().filter(|t| !t.is_empty()).and_then(parse_synced) {
                // Сдвиг «раньше» положителен, а редактор работает во времени трека.
                return LyricsDraft::from_lyrics(&synced).shifted_by(-stored.offset_ms);
            }
            if let Some(plain) = stored.plain.as_deref().filter(|t| !t.is_empty()) {
                return LyricsDraft::from_text(plain, stored.language.clone());
            }
        }
        LyricsDraft { language: stored.and_then(|s| s.language), ..Default::default() }
    }

    /// Текст из редактора — свой (Android `saveLyricsDraft`): синхронный — TTML, если отмечена хоть
    /// одна строка, обычный рядом; чего в черновике нет, остаётся как было.
    pub fn save_draft(&self, video_id: &str, current: Option<&StoredLyrics>, draft: &LyricsDraft) {
        let synced = draft.to_synced().map(|lyrics| ttml::write(&lyrics));
        let plain = Some(draft.to_text()).filter(|t| !t.trim().is_empty());
        let saved = StoredLyrics {
            synced_source: if synced.is_some() { Some(sources::USER.into()) } else { current.and_then(|c| c.synced_source.clone()) },
            synced_ref: if synced.is_some() { None } else { current.and_then(|c| c.synced_ref.clone()) },
            plain_source: if plain.is_some() { Some(sources::USER.into()) } else { current.and_then(|c| c.plain_source.clone()) },
            plain_ref: if plain.is_some() { None } else { current.and_then(|c| c.plain_ref.clone()) },
            // Редактор пишет время трека: сдвига больше нет.
            offset_ms: if synced.is_some() { 0 } else { current.map(|c| c.offset_ms).unwrap_or(0) },
            language: draft.language.clone().or_else(|| current.and_then(|c| c.language.clone())),
            chosen: current.is_some_and(|c| c.chosen),
            synced: synced.clone().or_else(|| current.and_then(|c| c.synced.clone())),
            plain: plain.or_else(|| current.and_then(|c| c.plain.clone())),
        };
        if synced.is_some() {
            self.ctx.settings.set(&keys::LYRICS_VIEW, "synced".to_owned());
        }
        if self.track().is_some_and(|t| t.video_id == video_id) {
            self.update(saved);
        } else {
            self.save(video_id, saved);
        }
    }
}

/// Подпись источника под текстом.
pub fn source_text(source: Option<&str>) -> Option<&'static str> {
    use crate::localization::tr;
    Some(tr(match source? {
        sources::YOUTUBE_MUSIC => "LyricsSourceYouTubeMusic",
        sources::LRCLIB => "LyricsSourceLrcLib",
        sources::KUGOU => "LyricsSourceKuGou",
        sources::FILE => "LyricsSourceFile",
        sources::USER => "LyricsSourceUser",
        sources::MELOGOLD => "LyricsSourceMelogold",
        _ => return None,
    }))
}
