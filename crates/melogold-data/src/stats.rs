//! Итоги: статистика прослушиваний за неделю, месяц, год и всё время (задание 0009, Android
//! `ListeningStats`). Считается на устройстве по своей истории: события со всех устройств аккаунта, что
//! принёс синк, и свои. Границы периодов — по местному часовому поясу ([`melogold_core::stats_window`]).

use std::collections::{BTreeMap, HashMap};

use melogold_core::music::Track;
use melogold_core::stats_window::{self as calendar, Period, Window, Zone};
use rusqlite::params;

use crate::database::DbError;
use crate::library::{read_track, DeviceFilter, Library, TRACK_COLUMNS};
use crate::overrides::TrackOverride;

/// Сколько записей в топах («Показать все» — до этого числа).
pub const TOP_LIMIT: usize = 50;
/// Сколько открытий показывать.
pub const DISCOVERIES_SHOWN: usize = 5;

/// Прослушивание, как его читает подсчёт: что, когда и сколько.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatEvent {
    pub video_id: String,
    pub played_at: i64,
    pub play_time_ms: i64,
}

/// Всё, что читает подсчёт.
#[derive(Default)]
pub struct StatsInput {
    /// События от начала прошлого периода (для сравнения) до конца этого.
    pub events: Vec<StatEvent>,
    /// Треки событий этого периода.
    pub tracks: HashMap<String, Track>,
    pub overrides: HashMap<String, TrackOverride>,
    /// Когда трек прослушан впервые, на любом устройстве.
    pub first_plays: HashMap<String, i64>,
}

/// Трек топа: название и исполнитель с учётом своих правок; `track` — исходный, чтобы играть.
#[derive(Clone, Debug, PartialEq)]
pub struct TopTrack {
    pub track: Track,
    pub title: String,
    pub artist: Option<String>,
    pub plays: i64,
    pub ms: i64,
}

/// Исполнитель топа. `id` — `browseId` на YouTube Music; у имени, которое написал сам пользователь, его нет.
#[derive(Clone, Debug, PartialEq)]
pub struct TopArtist {
    pub id: Option<String>,
    pub name: String,
    pub thumbnail_url: Option<String>,
    pub plays: i64,
    pub ms: i64,
    pub tracks: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TopAlbum {
    pub id: Option<String>,
    pub title: String,
    pub thumbnail_url: Option<String>,
    pub plays: i64,
    pub ms: i64,
    pub tracks: usize,
}

/// Столбец «Когда вы слушали»: день, месяц или год, что начинается в `day` (дни с 1970-01-01).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bar {
    pub day: i64,
    pub ms: i64,
}

/// Треки, впервые услышанные в периоде, и самые слушаемые из них.
#[derive(Clone, Debug, PartialEq)]
pub struct Discoveries {
    pub count: usize,
    pub top: Vec<TopTrack>,
}

/// Время суток «Времени суток».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DayPart {
    Night,
    Morning,
    Afternoon,
    Evening,
}

impl DayPart {
    pub const ALL: [DayPart; 4] = [DayPart::Night, DayPart::Morning, DayPart::Afternoon, DayPart::Evening];

    /// Время суток, в которое попадает час 0–23.
    pub fn of_hour(hour: usize) -> DayPart {
        Self::ALL.into_iter().find(|part| part.hours().contains(&hour)).unwrap_or(DayPart::Night)
    }

    /// Часы времени суток: ночь 0–5, утро 6–11, день 12–17, вечер 18–23.
    pub fn hours(self) -> std::ops::Range<usize> {
        match self {
            DayPart::Night => 0..6,
            DayPart::Morning => 6..12,
            DayPart::Afternoon => 12..18,
            DayPart::Evening => 18..24,
        }
    }
}

/// Итоги одного периода.
#[derive(Clone, Debug, PartialEq)]
pub struct ListeningStats {
    pub window: Window,
    pub total_ms: i64,
    pub plays: i64,
    pub tracks: usize,
    pub artists: usize,
    pub albums: usize,
    /// Сколько играло в прошлом таком же периоде; у «Всё время» — `None`.
    pub previous_ms: Option<i64>,
    pub top_tracks: Vec<TopTrack>,
    pub top_artists: Vec<TopArtist>,
    pub top_albums: Vec<TopAlbum>,
    /// Дни недели или месяца, 12 месяцев года, годы «Всё время».
    pub bars: Vec<Bar>,
    /// Сколько играло в каждый из 24 часов.
    pub hours: [i64; 24],
    /// У «Всё время» — `None`: всё было когда-то новым.
    pub discoveries: Option<Discoveries>,
    /// Первое прослушивание в истории, на любом устройстве.
    pub earliest: Option<i64>,
}

impl ListeningStats {
    pub fn is_empty(&self) -> bool {
        self.plays == 0
    }

    /// «+12 %» к прошлому периоду; `None` без него или когда в нём ничего не играло.
    pub fn change_percent(&self) -> Option<i64> {
        percent_change(self.total_ms, self.previous_ms)
    }

    /// Самый слушаемый день, месяц или год.
    pub fn busiest_bar(&self) -> Option<Bar> {
        // Первый из равных.
        self.bars.iter().copied().fold(None, |best: Option<Bar>, bar| match best {
            Some(b) if b.ms >= bar.ms => Some(b),
            _ => (bar.ms > 0).then_some(bar).or(best),
        })
    }

    pub fn favorite_day_part(&self) -> Option<DayPart> {
        favorite_day_part(&self.hours)
    }

    /// Час, в который слушали больше всего.
    pub fn peak_hour(&self) -> Option<usize> {
        let (index, value) = self.hours.iter().copied().enumerate().fold((0, 0), |best, (i, v)| if v > best.1 { (i, v) } else { best });
        (value > 0).then_some(index)
    }

    /// Целых минут — «23 104 минуты музыки за год».
    pub fn minutes(&self) -> i64 {
        self.total_ms / 60_000
    }

    /// Есть ли что-то раньше этого периода: история начинается до него.
    pub fn has_earlier(&self) -> bool {
        self.window.period != Period::AllTime && self.earliest.is_some_and(|first| self.window.start_ms > first)
    }
}

/// Насколько `current` больше (меньше) `previous` в целых процентах.
pub fn percent_change(current: i64, previous: Option<i64>) -> Option<i64> {
    let previous = previous.filter(|p| *p > 0)?;
    Some(((current - previous) as f64 * 100.0 / previous as f64).round() as i64)
}

pub fn favorite_day_part(hours: &[i64; 24]) -> Option<DayPart> {
    let mut best: Option<(DayPart, i64)> = None;
    for part in DayPart::ALL {
        let sum: i64 = part.hours().map(|h| hours[h]).sum();
        if sum > 0 && best.is_none_or(|(_, b)| sum > b) {
            best = Some((part, sum));
        }
    }
    best.map(|(part, _)| part)
}

/// Карточки «Итогов года» по порядку (задание 0009).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrappedCard {
    Minutes,
    TrackOfYear,
    TopArtists,
    TopTracks,
    FavoriteTime,
    Discoveries,
}

/// Карточки, которым есть что сказать по итогам года: пустой год — ни одной, без открытий их пять.
pub fn wrapped_cards(stats: &ListeningStats) -> Vec<WrappedCard> {
    let mut cards = Vec::new();
    if stats.is_empty() {
        return cards;
    }
    cards.push(WrappedCard::Minutes);
    if !stats.top_tracks.is_empty() {
        cards.push(WrappedCard::TrackOfYear);
    }
    if !stats.top_artists.is_empty() {
        cards.push(WrappedCard::TopArtists);
    }
    if !stats.top_tracks.is_empty() {
        cards.push(WrappedCard::TopTracks);
    }
    cards.push(WrappedCard::FavoriteTime);
    if stats.discoveries.as_ref().is_some_and(|d| d.count > 0) {
        cards.push(WrappedCard::Discoveries);
    }
    cards
}

const ARTIST_SEPARATORS: [&str; 4] = [", ", " & ", " feat. ", " ft. "];

/// Первый исполнитель строки: «Noize MC feat. Oxxxymiron» — «Noize MC».
pub fn first_artist(artists_text: Option<&str>) -> Option<String> {
    let text = artists_text?.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let mut end = text.len();
    for separator in ARTIST_SEPARATORS {
        // Регистр не меняет длину в байтах у ASCII-разделителей; позицию берём по нижнему регистру.
        if let Some(at) = lower.find(separator) {
            if at > 0 && at < end && text.is_char_boundary(at) {
                end = at;
            }
        }
    }
    Some(text[..end].trim().to_owned()).filter(|s| !s.is_empty())
}

fn key_of(name: &str) -> String {
    name.trim().to_lowercase()
}

#[derive(Default)]
struct Tally {
    plays: i64,
    ms: i64,
}

/// Исполнитель или альбом, пока считается: картинка — обложка самого слушаемого трека.
struct Group {
    id: Option<String>,
    name: String,
    plays: i64,
    ms: i64,
    tracks: usize,
    cover: Option<String>,
}

impl Group {
    fn new(id: Option<String>, name: &str) -> Group {
        Group { id, name: name.to_owned(), plays: 0, ms: 0, tracks: 0, cover: None }
    }

    fn add(&mut self, track: &TopTrack, id: Option<String>) {
        self.plays += track.plays;
        self.ms += track.ms;
        self.tracks += 1;
        if self.id.is_none() {
            self.id = id;
        }
        if self.cover.is_none() {
            self.cover = track.track.thumbnail_url.clone();
        }
    }
}

type Groups = Vec<Group>;

fn add_to(groups: &mut Groups, index: &mut HashMap<String, usize>, name: &str, id: Option<String>, track: &TopTrack) {
    let at = *index.entry(key_of(name)).or_insert_with(|| {
        groups.push(Group::new(None, name));
        groups.len() - 1
    });
    groups[at].add(track, id);
}

fn sorted_for_top(mut groups: Groups) -> Groups {
    groups.sort_by(|a, b| b.ms.cmp(&a.ms).then(b.plays.cmp(&a.plays)).then_with(|| a.name.cmp(&b.name)));
    groups
}

/// Посчитать `window`. Прослушивание относится к периоду, в который попало его время по поясу `zone`;
/// прошлый период считается из тех же событий — для сравнения.
///
/// Исполнитель трека — по порядку: написанный пользователем; первый из `artists` (с ссылкой на его
/// страницу); первый из `artists_text`. Альбом — написанный пользователем, иначе альбом трека, иначе
/// трек в топ альбомов не идёт. Имя, которое написал пользователь, объединяется с одноимённым.
pub fn compute(input: &StatsInput, window: Window, zone: Zone, today: i64) -> ListeningStats {
    let previous = window.previous(zone);
    let (mut previous_ms, mut total_ms, mut plays) = (0i64, 0i64, 0i64);
    let mut per_track: HashMap<&str, Tally> = HashMap::new();
    let mut hours = [0i64; 24];
    let first_day = window.first_day.unwrap_or(0);
    let mut by_index = match window.period {
        Period::Week | Period::Month => vec![0i64; window.days() as usize],
        Period::Year => vec![0i64; 12],
        Period::AllTime => Vec::new(),
    };
    let mut by_year: BTreeMap<i64, i64> = BTreeMap::new();

    for event in &input.events {
        let time = event.play_time_ms.max(0);
        if previous.is_some_and(|p| p.contains(event.played_at)) {
            previous_ms += time;
            continue;
        }
        if !window.contains(event.played_at) {
            continue;
        }
        total_ms += time;
        plays += 1;
        let tally = per_track.entry(event.video_id.as_str()).or_default();
        tally.plays += 1;
        tally.ms += time;
        hours[calendar::local_hour(event.played_at, zone)] += time;
        let day = calendar::local_day(event.played_at, zone);
        match window.period {
            Period::Week | Period::Month => {
                if let Some(slot) = usize::try_from(day - first_day).ok().and_then(|i| by_index.get_mut(i)) {
                    *slot += time;
                }
            }
            Period::Year => {
                let (_, month, _) = calendar::civil_from_days(day);
                by_index[month as usize - 1] += time;
            }
            Period::AllTime => *by_year.entry(calendar::civil_from_days(day).0).or_default() += time,
        }
    }

    let mut ranked: Vec<TopTrack> = per_track
        .iter()
        .filter_map(|(id, tally)| {
            let track = input.tracks.get(*id)?;
            let edit = input.overrides.get(*id);
            Some(TopTrack {
                track: track.clone(),
                title: edit.and_then(|e| e.title.clone()).unwrap_or_else(|| track.title.clone()),
                artist: edit.and_then(|e| e.artists_text.clone()).or_else(|| track.artists_text.clone()),
                plays: tally.plays,
                ms: tally.ms,
            })
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.ms.cmp(&a.ms)
            .then(b.plays.cmp(&a.plays))
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.track.video_id.cmp(&b.track.video_id))
    });

    let (mut artist_groups, mut album_groups): (Groups, Groups) = (Vec::new(), Vec::new());
    let (mut artist_index, mut album_index) = (HashMap::new(), HashMap::new());
    for top in &ranked {
        let track = &top.track;
        let edit = input.overrides.get(&track.video_id);
        let listed = track.artists.iter().find(|a| !a.name.trim().is_empty());
        let artist = match edit.and_then(|e| e.artists_text.as_deref()) {
            Some(written) => first_artist(Some(written)),
            None => listed.map(|a| a.name.trim().to_owned()).or_else(|| first_artist(track.artists_text.as_deref())),
        };
        if let Some(name) = artist {
            // Ссылка на страницу исполнителя остаётся, только пока имя то, что дал YouTube.
            let id = listed.filter(|a| key_of(&a.name) == key_of(&name)).and_then(|a| a.id.clone());
            add_to(&mut artist_groups, &mut artist_index, &name, id, top);
        }
        let album = edit.and_then(|e| e.album_title.clone()).or_else(|| track.album_title.clone()).filter(|t| !t.trim().is_empty());
        if let Some(title) = album {
            let id = match edit.and_then(|e| e.album_title.as_deref()) {
                Some(written) if track.album_title.as_deref().map(key_of) != Some(key_of(written)) => None,
                _ => track.album_id.clone(),
            };
            add_to(&mut album_groups, &mut album_index, title.trim(), id, top);
        }
    }
    let artists = sorted_for_top(artist_groups);
    let albums = sorted_for_top(album_groups);

    let discoveries = (window.period != Period::AllTime).then(|| {
        let fresh: Vec<&TopTrack> =
            ranked.iter().filter(|t| input.first_plays.get(&t.track.video_id).is_some_and(|at| window.contains(*at))).collect();
        Discoveries { count: fresh.len(), top: fresh.into_iter().take(DISCOVERIES_SHOWN).cloned().collect() }
    });

    let bars = match window.period {
        Period::Week | Period::Month => by_index.iter().enumerate().map(|(i, ms)| Bar { day: first_day + i as i64, ms: *ms }).collect(),
        Period::Year => {
            let (year, ..) = calendar::civil_from_days(first_day);
            by_index.iter().enumerate().map(|(i, ms)| Bar { day: calendar::days_from_civil(year, i as u32 + 1, 1), ms: *ms }).collect()
        }
        Period::AllTime => match by_year.keys().next() {
            None => Vec::new(),
            Some(first) => {
                let last = (*by_year.keys().next_back().unwrap_or(first)).max(calendar::civil_from_days(today).0);
                (*first..=last)
                    .map(|y| Bar { day: calendar::days_from_civil(y, 1, 1), ms: by_year.get(&y).copied().unwrap_or(0) })
                    .collect()
            }
        },
    };

    ListeningStats {
        window,
        total_ms,
        plays,
        tracks: ranked.len(),
        artists: artists.len(),
        albums: albums.len(),
        previous_ms: previous.map(|_| previous_ms),
        top_tracks: ranked.iter().take(TOP_LIMIT).cloned().collect(),
        top_artists: artists
            .into_iter()
            .take(TOP_LIMIT)
            .map(|g| TopArtist { id: g.id, name: g.name, thumbnail_url: g.cover, plays: g.plays, ms: g.ms, tracks: g.tracks })
            .collect(),
        top_albums: albums
            .into_iter()
            .take(TOP_LIMIT)
            .map(|g| TopAlbum { id: g.id, title: g.name, thumbnail_url: g.cover, plays: g.plays, ms: g.ms, tracks: g.tracks })
            .collect(),
        bars,
        hours,
        discoveries,
        earliest: input.first_plays.values().copied().min(),
    }
}

impl Library {
    /// Итоги `window` для устройств `filter`: события за него и прошлый период, треки периода, свои
    /// правки и первые прослушивания. Работает с диском — не из главного потока.
    pub fn listening_stats(&self, window: Window, zone: Zone, today: i64, filter: &DeviceFilter) -> Result<ListeningStats, DbError> {
        let from = window.previous(zone).map_or(window.start_ms, |p| p.start_ms);
        let (condition, device) = filter.sql();
        let input = self.database().read(|c| {
            let mut events = Vec::new();
            {
                let mut statement = c.prepare(&format!(
                    "SELECT video_id, played_at, play_time_ms FROM play_events WHERE played_at >= ?1 AND played_at < ?3 AND {condition}"
                ))?;
                let rows = statement.query_map(params![from, device, window.end_ms], |r| {
                    Ok(StatEvent { video_id: r.get(0)?, played_at: r.get(1)?, play_time_ms: r.get(2)? })
                })?;
                for row in rows {
                    events.push(row?);
                }
            }
            let mut tracks = HashMap::new();
            {
                let mut statement = c.prepare(&format!(
                    "SELECT {TRACK_COLUMNS} FROM tracks WHERE video_id IN
                       (SELECT DISTINCT video_id FROM play_events WHERE played_at >= ?1 AND played_at < ?3 AND {condition})"
                ))?;
                let rows = statement.query_map(params![window.start_ms, device, window.end_ms], |r| read_track(r, 0))?;
                for row in rows {
                    let track = row?;
                    tracks.insert(track.video_id.clone(), track);
                }
            }
            let mut first_plays = HashMap::new();
            {
                let mut statement = c.prepare("SELECT video_id, MIN(played_at) FROM play_events GROUP BY video_id")?;
                let rows = statement.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
                for row in rows {
                    let (id, at) = row?;
                    first_plays.insert(id, at);
                }
            }
            Ok(StatsInput { events, tracks, overrides: HashMap::new(), first_plays })
        })?;
        let input = StatsInput { overrides: self.track_overrides(), ..input };
        Ok(compute(&input, window, zone, today))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use melogold_core::music::ArtistRef;
    use melogold_core::stats_window::{days_from_civil, window, DAY_MS, HOUR_MS};

    use super::*;
    use crate::Database;

    fn utc(_: i64) -> i64 {
        0
    }
    fn plus3(_: i64) -> i64 {
        3 * HOUR_MS
    }
    fn at(y: i64, m: u32, d: u32, hour: i64) -> i64 {
        days_from_civil(y, m, d) * DAY_MS + hour * HOUR_MS
    }
    fn today() -> i64 {
        days_from_civil(2026, 9, 30)
    }
    fn library() -> Arc<Library> {
        Library::open(Database::in_memory().unwrap())
    }
    fn track(id: &str, artists: &str) -> Track {
        Track { video_id: id.into(), title: format!("Трек {id}"), artists_text: Some(artists.into()), ..Default::default() }
    }
    fn play(lib: &Library, id: &str, when: i64, ms: i64, device: Option<&str>) {
        lib.sync(|tx| {
            tx.ensure_track(id, None)?;
            tx.insert_play(&format!("e-{id}-{when}-{}", device.unwrap_or("")), id, when, ms, device)
        })
        .unwrap();
    }
    fn stats(lib: &Library, period: Period, offset: i64, zone: Zone, filter: &DeviceFilter) -> ListeningStats {
        lib.listening_stats(window(period, offset, today(), zone), zone, today(), filter).unwrap()
    }

    #[test]
    fn first_artist_splits_the_line() {
        assert_eq!(first_artist(Some("Noize MC feat. Oxxxymiron")).as_deref(), Some("Noize MC"));
        assert_eq!(first_artist(Some("Кино & Nautilus")).as_deref(), Some("Кино"));
        assert_eq!(first_artist(Some("A, B и C")).as_deref(), Some("A"));
        assert_eq!(first_artist(Some("  Zivert ft. Артик ")).as_deref(), Some("Zivert"));
        assert_eq!(first_artist(Some("  ")), None);
        assert_eq!(first_artist(None), None);
    }

    #[test]
    fn period_bounds_in_the_device_time_zone() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино")]).unwrap();
        // 31 августа 22:00 UTC — в Москве уже 1 сентября (месяц), а в UTC ещё август.
        play(&lib, "a", at(2026, 8, 31, 22), 60_000, None);
        // 30 сентября 21:30 UTC — в Москве уже 1 октября: в сентябрь не входит.
        play(&lib, "a", at(2026, 9, 30, 21) + 1_800_000, 30_000, None);
        // 27 сентября 22:00 UTC — воскресенье в UTC, но уже понедельник 28-го в Москве.
        play(&lib, "a", at(2026, 9, 27, 22), 5_000, None);
        assert_eq!(stats(&lib, Period::Month, 0, &plus3, &DeviceFilter::All).total_ms, 65_000);
        assert_eq!(stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All).total_ms, 35_000);
        // Неделя с понедельника 28 сентября: в Москве в неё входят оба события, в UTC — одно.
        assert_eq!(stats(&lib, Period::Week, 0, &utc, &DeviceFilter::All).total_ms, 30_000);
        assert_eq!(stats(&lib, Period::Week, 0, &plus3, &DeviceFilter::All).total_ms, 35_000);
        // Год: обе даты в 2026-м.
        assert_eq!(stats(&lib, Period::Year, 0, &utc, &DeviceFilter::All).plays, 3);
        // Всё время: без сравнения и без открытий.
        let all = stats(&lib, Period::AllTime, 0, &utc, &DeviceFilter::All);
        assert_eq!((all.plays, all.previous_ms, all.discoveries.is_none()), (3, None, true));
        assert_eq!(all.bars.iter().map(|b| b.ms).collect::<Vec<_>>(), [95_000]);
    }

    #[test]
    fn week_edges_belong_to_the_right_week() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино")]).unwrap();
        play(&lib, "a", at(2026, 9, 27, 23), 10_000, None); // воскресенье прошлой недели
        play(&lib, "a", at(2026, 9, 28, 0), 20_000, None); // понедельник, полночь
        let week = stats(&lib, Period::Week, 0, &utc, &DeviceFilter::All);
        assert_eq!((week.total_ms, week.previous_ms), (20_000, Some(10_000)));
        assert_eq!(week.bars.len(), 7);
        assert_eq!(week.bars[0].ms, 20_000);
        assert_eq!(week.change_percent(), Some(100));
    }

    #[test]
    fn comparison_with_the_previous_period() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино")]).unwrap();
        play(&lib, "a", at(2026, 8, 10, 12), 100_000, None);
        play(&lib, "a", at(2026, 9, 10, 12), 112_000, None);
        let month = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!(month.previous_ms, Some(100_000));
        assert_eq!(month.change_percent(), Some(12));
        // В прошлом периоде пусто: сравнения нет, но и «0 %» не выдумывается.
        assert_eq!(stats(&lib, Period::Month, -1, &utc, &DeviceFilter::All).change_percent(), None);
        assert_eq!(stats(&lib, Period::Month, -2, &utc, &DeviceFilter::All).previous_ms, Some(0));
        assert_eq!(percent_change(50, Some(100)), Some(-50));
    }

    #[test]
    fn artist_without_a_map_comes_from_the_line() {
        let lib = library();
        lib.save_tracks(&[track("a", "Noize MC feat. Oxxxymiron"), track("b", "noize mc"), track("c", "Кино, Ассаи")]).unwrap();
        for (id, ms) in [("a", 90_000), ("b", 60_000), ("c", 30_000)] {
            play(&lib, id, at(2026, 9, 5, 10), ms, None);
        }
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!(s.artists, 2, "«Noize MC» и «noize mc» — один исполнитель");
        assert_eq!((s.top_artists[0].name.as_str(), s.top_artists[0].ms, s.top_artists[0].tracks), ("Noize MC", 150_000, 2));
        assert_eq!(s.top_artists[1].name, "Кино");
        assert!(s.top_artists[0].id.is_none());
        assert!(s.top_albums.is_empty(), "без альбома трек в топ альбомов не идёт");
    }

    #[test]
    fn artist_map_wins_over_the_line() {
        let lib = library();
        let mut t = track("a", "Кино & Кто-то");
        t.artists = vec![ArtistRef { id: Some("UC1".into()), name: "Кино".into() }];
        lib.save_tracks(&[t]).unwrap();
        play(&lib, "a", at(2026, 9, 5, 10), 60_000, None);
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!((s.top_artists[0].name.as_str(), s.top_artists[0].id.as_deref()), ("Кино", Some("UC1")));
        // Своё имя того же исполнителя — та же страница; чужое имя — без страницы.
        lib.set_override("a", None, Some("кино"), None).unwrap();
        assert_eq!(stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All).top_artists[0].id.as_deref(), Some("UC1"));
        lib.set_override("a", None, Some("Другой feat. Кино"), None).unwrap();
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!((s.top_artists[0].name.as_str(), s.top_artists[0].id.clone()), ("Другой", None));
    }

    #[test]
    fn album_edit_moves_the_track() {
        let lib = library();
        let mut a = track("a", "Кино");
        a.album_title = Some("Группа крови".into());
        a.album_id = Some("MPREb_1".into());
        lib.save_tracks(&[a, track("b", "Кино")]).unwrap();
        play(&lib, "a", at(2026, 9, 5, 10), 60_000, None);
        play(&lib, "b", at(2026, 9, 6, 10), 30_000, None);
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!((s.albums, s.top_albums[0].title.as_str(), s.top_albums[0].id.as_deref()), (1, "Группа крови", Some("MPREb_1")));
        // Правка альбома кладёт трек в этот альбом: «b» присоединяется к «Группа крови» (регистр не важен).
        lib.set_album(&["b".into()], "группа крови").unwrap();
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!((s.albums, s.top_albums[0].tracks, s.top_albums[0].ms), (1, 2, 90_000));
        // Правка «a» в другой альбом уводит его, и страница исходного альбома ему больше не нужна.
        lib.set_album(&["a".into()], "Звезда по имени Солнце").unwrap();
        let s = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All);
        assert_eq!(s.albums, 2);
        assert_eq!(s.top_albums[0].title, "Звезда по имени Солнце");
        assert_eq!(s.top_albums[0].id, None);
    }

    #[test]
    fn device_filter_counts_only_the_chosen_devices() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино"), track("b", "Кино")]).unwrap();
        play(&lib, "a", at(2026, 9, 5, 10), 10_000, None); // это устройство (без deviceId)
        play(&lib, "a", at(2026, 9, 6, 10), 20_000, Some("me")); // это устройство, вернулось с сервера
        play(&lib, "b", at(2026, 9, 7, 10), 40_000, Some("phone"));
        play(&lib, "b", at(2026, 9, 8, 10), 80_000, Some("")); // без deviceId с сервера
        let total = |filter: DeviceFilter| stats(&lib, Period::Month, 0, &utc, &filter).total_ms;
        assert_eq!(total(DeviceFilter::All), 150_000);
        assert_eq!(total(DeviceFilter::This(Some("me".into()))), 30_000);
        assert_eq!(total(DeviceFilter::Device("phone".into())), 40_000);
        // Открытия считают «впервые» по всем устройствам: «b» с телефона в фильтре «Это устройство» не появляется.
        let phone = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::Device("phone".into()));
        assert_eq!(phone.discoveries.unwrap().count, 1);
    }

    #[test]
    fn wrapped_cards_skip_what_has_nothing_to_say() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино")]).unwrap();
        assert!(wrapped_cards(&stats(&lib, Period::Year, 0, &utc, &DeviceFilter::All)).is_empty());
        play(&lib, "a", at(2026, 3, 1, 10), 90_000, None);
        let year = stats(&lib, Period::Year, 0, &utc, &DeviceFilter::All);
        assert_eq!(year.minutes(), 1);
        assert_eq!(wrapped_cards(&year).len(), 6, "трек — открытие года");
        // Год, где всё уже слышали раньше: без карточки открытий.
        play(&lib, "a", at(2025, 3, 1, 10), 90_000, None);
        let year = stats(&lib, Period::Year, 0, &utc, &DeviceFilter::All);
        assert_eq!(wrapped_cards(&year).last(), Some(&WrappedCard::FavoriteTime));
        assert_eq!(wrapped_cards(&year).len(), 5);
    }

    #[test]
    fn discoveries_are_tracks_first_heard_in_the_period() {
        let lib = library();
        lib.save_tracks(&[track("old", "Кино"), track("new", "Кино"), track("new2", "Кино")]).unwrap();
        play(&lib, "old", at(2026, 7, 1, 10), 10_000, None);
        play(&lib, "old", at(2026, 9, 2, 10), 500_000, None);
        play(&lib, "new", at(2026, 9, 3, 10), 30_000, None);
        play(&lib, "new2", at(2026, 9, 4, 10), 60_000, None);
        let d = stats(&lib, Period::Month, 0, &utc, &DeviceFilter::All).discoveries.unwrap();
        assert_eq!(d.count, 2);
        assert_eq!(d.top.iter().map(|t| t.track.video_id.as_str()).collect::<Vec<_>>(), ["new2", "new"]);
        // В августе никто не слушал: «old» там не открытие, потому что он был в июле.
        assert_eq!(stats(&lib, Period::Month, -1, &utc, &DeviceFilter::All).discoveries.unwrap().count, 0);
    }

    #[test]
    fn hours_and_bars() {
        let lib = library();
        lib.save_tracks(&[track("a", "Кино")]).unwrap();
        play(&lib, "a", at(2026, 9, 5, 21), 60_000, None); // в Москве — 00:00 6 сентября
        play(&lib, "a", at(2026, 9, 6, 8), 120_000, None);
        let s = stats(&lib, Period::Month, 0, &plus3, &DeviceFilter::All);
        assert_eq!((s.hours[0], s.hours[11]), (60_000, 120_000));
        assert_eq!(s.favorite_day_part(), Some(DayPart::Morning));
        assert_eq!(s.peak_hour(), Some(11));
        assert_eq!(s.bars.len(), 30);
        assert_eq!((s.bars[5].ms, s.busiest_bar().unwrap().day), (180_000, days_from_civil(2026, 9, 6)));
        let year = stats(&lib, Period::Year, 0, &utc, &DeviceFilter::All);
        assert_eq!(year.bars.len(), 12);
        assert_eq!(year.bars[8].ms, 180_000);
        assert_eq!(year.busiest_bar().unwrap().day, days_from_civil(2026, 9, 1));
        assert!(stats(&lib, Period::Month, -1, &utc, &DeviceFilter::All).is_empty());
    }

    /// 50 000 событий — не дольше 300 мс (задание 0009).
    #[test]
    fn fifty_thousand_events_are_fast() {
        let lib = library();
        let tracks: Vec<Track> = (0..400)
            .map(|i| Track {
                album_title: Some(format!("Альбом {}", i % 40)),
                ..track(&format!("t{i}"), &format!("Исполнитель {}", i % 90))
            })
            .collect();
        lib.save_tracks(&tracks).unwrap();
        lib.sync(|tx| {
            for i in 0..50_000i64 {
                let id = format!("t{}", (i * 7919) % 400);
                tx.insert_play(
                    &format!("e{i}"),
                    &id,
                    at(2026, 1, 1, 0) + i * 500_000,
                    30_000 + i % 200_000,
                    (i % 3 == 0).then_some("phone"),
                )?;
            }
            Ok(())
        })
        .unwrap();
        let started = std::time::Instant::now();
        let s = lib.listening_stats(window(Period::Year, 0, today(), &plus3), &plus3, today(), &DeviceFilter::All).unwrap();
        let elapsed = started.elapsed();
        assert!(s.plays > 30_000, "{}", s.plays);
        eprintln!("итоги по 50 000 событий: {elapsed:?}");
        assert!(elapsed.as_millis() < 300, "{elapsed:?}");
    }
}
