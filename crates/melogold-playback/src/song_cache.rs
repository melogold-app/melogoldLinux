//! Кэш музыки (задание Windows 0003, docs/PROMPT.md §4 «Кэш музыки»), порт `SongCache.cs`.
//!
//! Всё, что играет, пишется кусками по мере чтения; повтор идёт с диска, а трек, прочитанный
//! целиком, играет без сети и без единого запроса. Ключ — `videoId`, один формат на трек. Сверх
//! лимита уходят треки, которые дольше всех не слушали; играющий не трогается.
//!
//! На трек — `<videoId>.<itag>.data` (байты по своим смещениям) и `.json` (сведения о потоке без
//! адреса, длина, прочитанные диапазоны). Индекс держится в памяти: читается с диска один раз.
//! Тот же тип служит загрузкам — своя папка, без лимита.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::resolver::StreamInfo;

type Listener = Box<dyn Fn(&str) + Send + Sync>;

pub struct SongCache {
    dir: PathBuf,
    /// Лимит в байтах; 0 — без ограничений.
    max_bytes: AtomicI64,
    index: Mutex<Option<HashMap<String, Arc<Entry>>>>,
    listener: Mutex<Option<Listener>>,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    info: StreamInfo,
    total: Option<u64>,
    ranges: Vec<[u64; 2]>,
}

impl SongCache {
    pub fn new(dir: PathBuf, max_bytes: i64) -> Arc<Self> {
        Arc::new(Self { dir, max_bytes: AtomicI64::new(max_bytes), index: Mutex::default(), listener: Mutex::default() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn set_max_bytes(&self, bytes: i64) {
        self.max_bytes.store(bytes, Ordering::Relaxed);
        self.trim();
    }

    /// Трек стал целиком в кэше или ушёл из него: метки «есть без сети» обновляются.
    pub fn set_listener(&self, listener: impl Fn(&str) + Send + Sync + 'static) {
        if let Ok(mut slot) = self.listener.lock() {
            *slot = Some(Box::new(listener));
        }
    }

    fn notify(&self, video_id: &str) {
        if let Ok(slot) = self.listener.lock() {
            if let Some(listener) = slot.as_ref() {
                listener(video_id);
            }
        }
    }

    fn with_index<T>(&self, f: impl FnOnce(&mut HashMap<String, Arc<Entry>>) -> T) -> T {
        let mut guard = self.index.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = guard.get_or_insert_with(|| self.read_index());
        f(index)
    }

    fn read_index(&self) -> HashMap<String, Arc<Entry>> {
        let mut index: HashMap<String, Arc<Entry>> = HashMap::new();
        let Ok(dir) = std::fs::read_dir(&self.dir) else { return index };
        for file in dir.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let modified = file.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
            let Some(entry) = Entry::read(&path.with_extension(""), modified) else { continue };
            match index.get(&entry.info.video_id) {
                // Два формата одного трека: остаётся тот, что читали позже.
                Some(other) if other.last_read() >= entry.last_read() => entry.delete_files(),
                other => {
                    if let Some(other) = other {
                        other.delete_files();
                    }
                    index.insert(entry.info.video_id.clone(), Arc::new(entry));
                }
            }
        }
        index
    }

    /// Занято на диске, байт.
    pub fn size(&self) -> u64 {
        self.with_index(|index| index.values().map(|e| e.disk_bytes()).sum())
    }

    /// Запись для потока и отметка «играет»: пока её не отпустят, не вытесняется.
    pub fn entry(&self, info: &StreamInfo) -> Arc<Entry> {
        let entry = self.with_index(|index| {
            let entry = match index.get(&info.video_id) {
                Some(known)
                    if known.info.itag == info.itag
                        && (info.content_length.is_none() || known.total().is_none() || known.total() == info.content_length) =>
                {
                    Arc::clone(known)
                }
                other => {
                    // Другой формат или другая длина: старые байты трека уходят.
                    if let Some(old) = other {
                        old.delete_files();
                    }
                    let entry = Arc::new(Entry::create(self.dir.join(format!("{}.{}", info.video_id, info.itag)), info));
                    index.insert(info.video_id.clone(), Arc::clone(&entry));
                    entry
                }
            };
            entry.pins.fetch_add(1, Ordering::SeqCst);
            entry
        });
        entry.touch(true);
        self.trim();
        entry
    }

    /// Трек целиком в кэше: сведения о потоке для воспроизведения без сети.
    pub fn complete(&self, video_id: &str) -> Option<StreamInfo> {
        self.with_index(|index| index.get(video_id).filter(|e| e.is_complete()).map(|e| e.offline_info()))
    }

    pub fn is_complete(&self, video_id: &str) -> bool {
        self.with_index(|index| index.get(video_id).is_some_and(|e| e.is_complete()))
    }

    /// Треки целиком в кэше, сначала недавно слушанные («Скачанное» › «В кэше»).
    pub fn complete_tracks(&self) -> Vec<(String, u64)> {
        self.with_index(|index| {
            let mut complete: Vec<_> = index.values().filter(|e| e.is_complete()).collect();
            complete.sort_by_key(|e| std::cmp::Reverse(e.last_read()));
            complete.iter().map(|e| (e.info.video_id.clone(), e.disk_bytes())).collect()
        })
    }

    /// Сверх лимита — удалить треки, которые дольше всех не слушали; играющие остаются.
    pub fn trim(&self) {
        let max = self.max_bytes.load(Ordering::Relaxed);
        if max <= 0 {
            return;
        }
        let removed = self.with_index(|index| {
            let mut size: u64 = index.values().map(|e| e.disk_bytes()).sum();
            let mut by_age: Vec<Arc<Entry>> = index.values().cloned().collect();
            by_age.sort_by_key(|e| e.last_read());
            let mut removed = Vec::new();
            for entry in by_age {
                if size <= max as u64 {
                    break;
                }
                if entry.is_pinned() {
                    continue;
                }
                size = size.saturating_sub(entry.disk_bytes());
                entry.delete_files();
                index.remove(&entry.info.video_id);
                removed.push(entry.info.video_id.clone());
            }
            removed
        });
        for video_id in removed {
            self.notify(&video_id);
        }
    }

    /// «Очистить кэш»: всё, кроме играющего сейчас.
    pub fn clear(&self) {
        let removed = self.with_index(|index| {
            let unpinned: Vec<String> = index.values().filter(|e| !e.is_pinned()).map(|e| e.info.video_id.clone()).collect();
            for video_id in &unpinned {
                if let Some(entry) = index.remove(video_id) {
                    entry.delete_files();
                }
            }
            unpinned
        });
        for video_id in removed {
            self.notify(&video_id);
        }
    }

    /// Удалить трек; с `unless_playing` играющий остаётся.
    pub fn remove(&self, video_id: &str, unless_playing: bool) {
        let removed = self.with_index(|index| match index.get(video_id) {
            Some(entry) if !(unless_playing && entry.is_pinned()) => index.remove(video_id),
            _ => None,
        });
        if let Some(entry) = removed {
            entry.delete_files();
            self.notify(video_id);
        }
    }

    /// Треки, которые здесь есть (целиком или частично).
    pub fn video_ids(&self) -> Vec<String> {
        self.with_index(|index| index.keys().cloned().collect())
    }

    pub(crate) fn completed(&self, video_id: &str) {
        self.notify(video_id);
    }
}

/// Кэш одного трека: формат, длина, какие диапазоны байтов уже на диске, когда читали.
pub struct Entry {
    base: PathBuf,
    info: StreamInfo,
    state: Mutex<State>,
    pins: AtomicUsize,
}

struct State {
    ranges: Vec<(u64, u64)>,
    total: Option<u64>,
    last_read: SystemTime,
    disk_bytes: u64,
    deleted: bool,
}

impl Entry {
    fn create(base: PathBuf, info: &StreamInfo) -> Entry {
        for extension in ["data", "json"] {
            let _ = std::fs::remove_file(base.with_extension(extension_after(&base, extension)));
        }
        Entry {
            info: StreamInfo { url: String::new(), ..info.clone() },
            state: Mutex::new(State {
                ranges: Vec::new(),
                total: info.content_length,
                last_read: SystemTime::now(),
                disk_bytes: 0,
                deleted: false,
            }),
            pins: AtomicUsize::new(0),
            base,
        }
    }

    fn read(base: &Path, last_read: SystemTime) -> Option<Entry> {
        let data = data_path(base);
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(meta_path(base)).ok()?).ok()?;
        let disk_bytes = std::fs::metadata(&data).ok()?.len();
        let entry = Entry {
            base: base.to_owned(),
            info: meta.info,
            state: Mutex::new(State { ranges: Vec::new(), total: meta.total, last_read, disk_bytes, deleted: false }),
            pins: AtomicUsize::new(0),
        };
        {
            let mut state = entry.lock();
            for [start, end] in meta.ranges {
                add_range(&mut state.ranges, start, end.min(disk_bytes));
            }
        }
        Some(entry)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn video_id(&self) -> &str {
        &self.info.video_id
    }

    pub fn itag(&self) -> u32 {
        self.info.itag
    }

    pub fn total(&self) -> Option<u64> {
        self.lock().total
    }

    fn last_read(&self) -> SystemTime {
        self.lock().last_read
    }

    fn disk_bytes(&self) -> u64 {
        self.lock().disk_bytes
    }

    pub fn is_pinned(&self) -> bool {
        self.pins.load(Ordering::SeqCst) > 0
    }

    /// Трек больше не играет: его можно вытеснять.
    pub fn release(&self) {
        let _ = self.pins.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| Some(n.saturating_sub(1)));
    }

    /// Целиком: есть все байты от 0 до длины потока.
    pub fn is_complete(&self) -> bool {
        let state = self.lock();
        is_complete(&state)
    }

    fn offline_info(&self) -> StreamInfo {
        StreamInfo { content_length: self.total(), expires_at_ms: i64::MAX, ..self.info.clone() }
    }

    fn touch(&self, persist: bool) {
        let now = SystemTime::now();
        self.lock().last_read = now;
        if persist {
            if let Ok(file) = File::options().write(true).open(meta_path(&self.base)) {
                let _ = file.set_modified(now);
            }
        }
    }

    /// Диапазон целиком на диске — прочитать его.
    pub fn try_read(&self, start: u64, length: usize) -> Option<Vec<u8>> {
        let data = {
            let mut state = self.lock();
            let end = start + length as u64;
            if state.deleted || length == 0 || !state.ranges.iter().any(|(s, e)| *s <= start && *e >= end) {
                return None;
            }
            let read = || -> std::io::Result<Vec<u8>> {
                let mut file = File::open(data_path(&self.base))?;
                file.seek(SeekFrom::Start(start))?;
                let mut buffer = vec![0u8; length];
                file.read_exact(&mut buffer)?;
                Ok(buffer)
            };
            match read() {
                Ok(buffer) => buffer,
                Err(error) => {
                    tracing::warn!(%error, "кэш музыки не читается");
                    state.ranges.clear();
                    return None;
                }
            }
        };
        self.lock().last_read = SystemTime::now();
        Some(data)
    }

    /// Прочитанное из сети — сразу на диск; `total` — полная длина потока, если известна.
    /// Возвращает `true`, если трек этой записью стал целиком в кэше.
    pub fn write(&self, start: u64, data: &[u8], total: Option<u64>) -> bool {
        if data.is_empty() {
            return false;
        }
        let mut state = self.lock();
        if state.deleted {
            return false;
        }
        let was_complete = is_complete(&state);
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = self.base.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new().create(true).truncate(false).write(true).open(data_path(&self.base))?;
            file.seek(SeekFrom::Start(start))?;
            file.write_all(data)?;
            state.disk_bytes = file.metadata()?.len();
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "кэш музыки не записался");
            return false;
        }
        if total.is_some() {
            state.total = total;
        }
        add_range(&mut state.ranges, start, start + data.len() as u64);
        let meta = Meta { info: self.info.clone(), total: state.total, ranges: state.ranges.iter().map(|(s, e)| [*s, *e]).collect() };
        let temp = meta_path(&self.base).with_extension("json.tmp");
        if let Ok(text) = serde_json::to_string(&meta) {
            if std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, meta_path(&self.base))).is_err() {
                tracing::warn!("сведения кэша музыки не записались");
            }
        }
        state.last_read = SystemTime::now();
        !was_complete && is_complete(&state)
    }

    /// Удалить байты и сведения (вытеснение, смена формата, «Очистить кэш»).
    fn delete_files(&self) {
        {
            let mut state = self.lock();
            state.deleted = true;
            state.ranges.clear();
            state.disk_bytes = 0;
        }
        for path in [data_path(&self.base), meta_path(&self.base)] {
            if let Err(error) = std::fs::remove_file(&path) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(%error, файл = %path.display(), "кэш музыки не удалился");
                }
            }
        }
    }
}

fn extension_after(base: &Path, extension: &str) -> String {
    // `<videoId>.<itag>` — у пути уже есть «расширение» itag; дописываем, а не заменяем.
    match base.extension() {
        Some(itag) => format!("{}.{extension}", itag.to_string_lossy()),
        None => extension.to_owned(),
    }
}

fn data_path(base: &Path) -> PathBuf {
    base.with_extension(extension_after(base, "data"))
}

fn meta_path(base: &Path) -> PathBuf {
    base.with_extension(extension_after(base, "json"))
}

fn is_complete(state: &State) -> bool {
    matches!((state.total, state.ranges.as_slice()), (Some(total), [(0, end)]) if *end >= total)
}

/// Добавить `[start, end)` и слить соседние диапазоны.
fn add_range(ranges: &mut Vec<(u64, u64)>, start: u64, end: u64) {
    if end <= start {
        return;
    }
    ranges.push((start, end));
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (s, e) in ranges.drain(..) {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    *ranges = merged;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(video_id: &str, length: u64) -> StreamInfo {
        StreamInfo {
            video_id: video_id.into(),
            url: "https://example/videoplayback".into(),
            itag: 140,
            mime_type: "audio/mp4".into(),
            content_length: Some(length),
            bitrate: None,
            expires_at_ms: 0,
            source: "TEST".into(),
            user_agent: None,
            loudness_db: Some(1.5),
            duration_ms: Some(1000),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melogold-song-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn ranges_merge() {
        let mut ranges = Vec::new();
        add_range(&mut ranges, 10, 20);
        add_range(&mut ranges, 0, 5);
        add_range(&mut ranges, 5, 10);
        add_range(&mut ranges, 30, 40);
        assert_eq!(ranges, [(0, 20), (30, 40)]);
    }

    #[test]
    fn written_bytes_are_read_back_and_survive_restart() {
        let dir = temp_dir("restart");
        let cache = SongCache::new(dir.clone(), 0);
        let entry = cache.entry(&info("aaaaaaaaaaa", 10));
        assert!(entry.try_read(0, 4).is_none());
        assert!(!entry.write(4, b"efgh", Some(10)));
        assert!(!entry.write(0, b"abcd", None));
        assert_eq!(entry.try_read(2, 4).as_deref(), Some(&b"cdef"[..]));
        assert!(!cache.is_complete("aaaaaaaaaaa"));
        assert!(entry.write(8, b"ij", None), "последний кусок делает трек целым");
        assert!(cache.is_complete("aaaaaaaaaaa"));
        entry.release();

        let again = SongCache::new(dir.clone(), 0);
        let offline = again.complete("aaaaaaaaaaa").expect("целиком после перезапуска");
        assert!(offline.url.is_empty(), "адрес на диск не пишется");
        assert_eq!(offline.loudness_db, Some(1.5));
        assert_eq!(again.entry(&offline).try_read(0, 10).as_deref(), Some(&b"abcdefghij"[..]));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn trim_removes_oldest_but_not_playing() {
        let dir = temp_dir("trim");
        let cache = SongCache::new(dir.clone(), 25);
        let old = cache.entry(&info("oldoldoldol", 10));
        old.write(0, &[1; 10], None);
        old.release();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let playing = cache.entry(&info("playingplay", 10));
        playing.write(0, &[2; 10], None);
        std::thread::sleep(std::time::Duration::from_millis(20));
        let newer = cache.entry(&info("newernewern", 10));
        newer.write(0, &[3; 10], None);
        newer.release();
        cache.trim();
        let ids = cache.video_ids();
        assert!(!ids.contains(&"oldoldoldol".to_owned()), "{ids:?}");
        assert!(ids.contains(&"playingplay".to_owned()));
        cache.clear();
        assert_eq!(cache.video_ids(), ["playingplay"], "играющий «Очистить кэш» не трогает");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn other_format_replaces_old_bytes() {
        let dir = temp_dir("format");
        let cache = SongCache::new(dir.clone(), 0);
        let first = cache.entry(&info("formatforma", 10));
        first.write(0, &[1; 10], None);
        let mut other = info("formatforma", 8);
        other.itag = 139;
        let second = cache.entry(&other);
        assert!(second.try_read(0, 4).is_none());
        assert!(!cache.is_complete("formatforma"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
