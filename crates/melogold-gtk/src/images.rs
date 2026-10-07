//! Обложки (грабли §9 п. 6): до 320 px — `mqdefault`, а не 1280×720; грузятся параллельно и
//! кэшируются на диск (`$XDG_CACHE_HOME/melogold/images`) и в памяти (последние 300, но не больше 48 МБ
//! расшифрованных пикселей: крупные кадры 1280×720 — по 3,7 МБ). У кадра видео
//! чёрные поля срезаются при разборе (задание Windows 0007): обложка сингла из видео-«статики»
//! становится квадратом, превью 4:3 — кадром без полос. Разбор — не в главном потоке.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;

use gtk::{gdk, glib};
use sha2::{Digest, Sha256};

const MEMORY: usize = 300;

/// Предел расшифрованных обложек в памяти, байт (RGBA): картинку на экране держит и сам виджет.
const MEMORY_BYTES: usize = 48 * 1024 * 1024;

/// Кэш обложек сам освобождается каждые столько записанных картинок.
const TRIM_EVERY: u32 = 64;

#[derive(Clone)]
pub struct Images {
    dir: PathBuf,
    http: reqwest::Client,
    runtime: tokio::runtime::Handle,
    memory: Rc<RefCell<Memory>>,
    /// Предел кэша на диске, байт; 0 — без ограничения.
    max_bytes: Arc<AtomicI64>,
    written: Arc<AtomicU32>,
}

impl Images {
    pub fn new(dir: PathBuf, http: reqwest::Client, runtime: tokio::runtime::Handle, max_bytes: i64) -> Images {
        let images =
            Images { dir, http, runtime, memory: Rc::default(), max_bytes: Arc::new(AtomicI64::new(max_bytes)), written: Arc::default() };
        images.trim();
        images
    }

    /// Новый предел кэша (Настройки → «Максимальный размер»); лишнее уходит сразу.
    pub fn set_max_bytes(&self, bytes: i64) {
        self.max_bytes.store(bytes, Ordering::Relaxed);
        self.trim();
    }

    /// Занято на диске, байт; считается не в главном потоке.
    pub async fn size(&self) -> u64 {
        let dir = self.dir.clone();
        self.runtime.spawn_blocking(move || files(&dir).iter().map(|(_, size, _)| size).sum()).await.unwrap_or(0)
    }

    /// Кэш заполнился — место уступают картинки, которые дольше всех не показывали.
    pub fn trim(&self) {
        let (dir, max) = (self.dir.clone(), self.max_bytes.load(Ordering::Relaxed));
        if max <= 0 {
            return;
        }
        self.runtime.spawn_blocking(move || trim_dir(&dir, max as u64));
    }

    /// «Очистить»: всё с диска; в памяти показанное остаётся до перезапуска.
    pub fn clear(&self) {
        let dir = self.dir.clone();
        self.runtime.spawn_blocking(move || {
            for (path, _, _) in files(&dir) {
                let _ = std::fs::remove_file(path);
            }
        });
    }

    fn remember(&self, url: &str, texture: &gdk::Texture) {
        self.memory.borrow_mut().insert(url, texture);
    }

    pub fn cached(&self, url: &str) -> Option<gdk::Texture> {
        self.memory.borrow().map.get(url).cloned()
    }

    /// Картинка по адресу: из памяти, с диска или из сети. `None` — не удалось.
    pub async fn load(&self, url: String) -> Option<gdk::Texture> {
        if let Some(texture) = self.cached(&url) {
            return Some(texture);
        }
        let bytes = self.bytes(url.clone()).await?;
        let frame = melogold_core::thumbnails::is_video_frame(&url);
        let texture = self.runtime.spawn_blocking(move || decode(bytes, frame)).await.ok()??;
        self.remember(&url, &texture);
        Some(texture)
    }

    /// Байты картинки: с диска или из сети (заодно на диск).
    pub async fn bytes(&self, url: String) -> Option<Vec<u8>> {
        let path = self.dir.join(format!("{}.img", hex::encode(&Sha256::digest(url.as_bytes())[..16])));
        let (http, max, written, dir) = (self.http.clone(), Arc::clone(&self.max_bytes), Arc::clone(&self.written), self.dir.clone());
        self.runtime
            .spawn(async move {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    // Показали — картинка свежая: при заполнении кэша уходят те, что дольше не показывали.
                    if let Ok(file) = std::fs::File::options().append(true).open(&path) {
                        let _ = file.set_modified(std::time::SystemTime::now());
                    }
                    return Some(bytes);
                }
                let mut response = http.get(&url).send().await.ok()?;
                if response.status() == 404 {
                    // У старых видео нет крупных кадров — есть только hqdefault (Windows `Thumbnails.Fallback`).
                    let fallback = melogold_core::thumbnails::fallback(&url)?;
                    response = http.get(fallback).send().await.ok()?;
                }
                if !response.status().is_success() {
                    return None;
                }
                let bytes = response.bytes().await.ok()?.to_vec();
                if let Some(parent) = path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                let _ = tokio::fs::write(&path, &bytes).await;
                let max = max.load(Ordering::Relaxed);
                if max > 0 && written.fetch_add(1, Ordering::Relaxed) % TRIM_EVERY == TRIM_EVERY - 1 {
                    tokio::task::spawn_blocking(move || trim_dir(&dir, max as u64));
                }
                Some(bytes)
            })
            .await
            .ok()?
    }
}

/// Последние расшифрованные картинки: не больше [`MEMORY`] штук и [`MEMORY_BYTES`] байт.
#[derive(Default)]
struct Memory {
    map: HashMap<String, gdk::Texture>,
    order: VecDeque<String>,
    bytes: usize,
}

impl Memory {
    fn insert(&mut self, url: &str, texture: &gdk::Texture) {
        if self.map.insert(url.to_owned(), texture.clone()).is_some() {
            return;
        }
        self.order.push_back(url.to_owned());
        self.bytes += pixels(texture);
        // Последнюю — оставить, даже если она одна больше предела.
        while self.order.len() > 1 && (self.order.len() > MEMORY || self.bytes > MEMORY_BYTES) {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(texture) = self.map.remove(&old) {
                self.bytes = self.bytes.saturating_sub(pixels(&texture));
            }
        }
    }
}

fn pixels(texture: &gdk::Texture) -> usize {
    use gtk::prelude::*;
    texture.width().max(0) as usize * texture.height().max(0) as usize * 4
}

/// Картинка из байтов: без обводки скана, у кадра видео — ещё и без полей любого цвета (задание 0014).
fn decode(bytes: Vec<u8>, frame: bool) -> Option<gdk::Texture> {
    use gtk::prelude::*;
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let stride = width * 4;
    let mut data = vec![0u8; stride * height];
    // Формат выгрузки — ARGB32 cairo: B, G, R, A в памяти на little-endian; альфа не нужна.
    texture.download(&mut data, stride);
    let pixels = melogold_core::frame_bars::Pixels { data: &data, width, height, stride, channels: [0, 1, 2] };
    let Some(rect) = melogold_core::frame_bars::content(&pixels, frame) else { return Some(texture) };
    let mut cropped = Vec::with_capacity(rect.width * rect.height * 4);
    for y in rect.y..rect.y + rect.height {
        let start = y * stride + rect.x * 4;
        cropped.extend_from_slice(&data[start..start + rect.width * 4]);
    }
    let format =
        if cfg!(target_endian = "little") { gdk::MemoryFormat::B8g8r8a8Premultiplied } else { gdk::MemoryFormat::A8r8g8b8Premultiplied };
    Some(gdk::MemoryTexture::new(rect.width as i32, rect.height as i32, format, &glib::Bytes::from_owned(cropped), rect.width * 4).upcast())
}

/// Файлы кэша: путь, размер, когда показывали.
fn files(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            meta.is_file().then(|| (entry.path(), meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH)))
        })
        .collect()
}

fn trim_dir(dir: &Path, max: u64) {
    let mut all = files(dir);
    let mut total: u64 = all.iter().map(|(_, size, _)| size).sum();
    if total <= max {
        return;
    }
    all.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in all {
        if total <= max {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::prelude::*;

    fn texture(side: i32) -> gdk::Texture {
        let bytes = glib::Bytes::from_owned(vec![0u8; (side * side * 4) as usize]);
        gdk::MemoryTexture::new(side, side, gdk::MemoryFormat::R8g8b8a8, &bytes, side as usize * 4).upcast()
    }

    #[test]
    fn decoded_covers_stay_within_the_byte_budget() {
        let mut memory = Memory::default();
        // Кадр 1280×720 — около 3,5 МБ; 1024×1024 — ровно 4 МБ: в 48 МБ помещается 12.
        for index in 0..20 {
            memory.insert(&format!("big-{index}"), &texture(1024));
        }
        assert_eq!(memory.map.len(), 12);
        assert!(memory.bytes <= MEMORY_BYTES);
        assert!(memory.map.contains_key("big-19") && !memory.map.contains_key("big-7"), "уходят старые");
        // Маленьких по-прежнему не больше 300.
        for index in 0..400 {
            memory.insert(&format!("small-{index}"), &texture(16));
        }
        assert_eq!(memory.map.len(), MEMORY);
        assert_eq!(memory.order.len(), MEMORY);
        // Повторная вставка не считается дважды.
        let before = memory.bytes;
        memory.insert("small-399", &texture(16));
        assert_eq!(memory.bytes, before);
    }
}
