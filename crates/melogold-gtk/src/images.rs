//! Обложки (грабли §9 п. 6): до 320 px — `mqdefault`, а не 1280×720; грузятся параллельно и
//! кэшируются на диск (`$XDG_CACHE_HOME/melogold/images`) и в памяти (последние 300). У кадра видео
//! чёрные поля срезаются при разборе (задание Windows 0007): обложка сингла из видео-«статики»
//! становится квадратом, превью 4:3 — кадром без полос. Разбор — не в главном потоке.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::{gdk, glib};
use sha2::{Digest, Sha256};

const MEMORY: usize = 300;

#[derive(Clone)]
pub struct Images {
    dir: PathBuf,
    http: reqwest::Client,
    runtime: tokio::runtime::Handle,
    memory: Rc<RefCell<(HashMap<String, gdk::Texture>, VecDeque<String>)>>,
}

impl Images {
    pub fn new(dir: PathBuf, http: reqwest::Client, runtime: tokio::runtime::Handle) -> Images {
        Images { dir, http, runtime, memory: Rc::default() }
    }

    fn remember(&self, url: &str, texture: &gdk::Texture) {
        let mut memory = self.memory.borrow_mut();
        let (map, order) = &mut *memory;
        if map.insert(url.to_owned(), texture.clone()).is_none() {
            order.push_back(url.to_owned());
            while order.len() > MEMORY {
                if let Some(old) = order.pop_front() {
                    map.remove(&old);
                }
            }
        }
    }

    pub fn cached(&self, url: &str) -> Option<gdk::Texture> {
        self.memory.borrow().0.get(url).cloned()
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
        let http = self.http.clone();
        self.runtime
            .spawn(async move {
                if let Ok(bytes) = tokio::fs::read(&path).await {
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
                Some(bytes)
            })
            .await
            .ok()?
    }
}

/// Картинка из байтов; у кадра видео — без чёрных полей.
fn decode(bytes: Vec<u8>, frame: bool) -> Option<gdk::Texture> {
    use gtk::prelude::*;
    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
    if !frame {
        return Some(texture);
    }
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    let stride = width * 4;
    let mut data = vec![0u8; stride * height];
    // Формат выгрузки — ARGB32 cairo: B, G, R, A в памяти на little-endian; альфа не нужна.
    texture.download(&mut data, stride);
    let pixels = melogold_core::frame_bars::Pixels { data: &data, width, height, stride, channels: [0, 1, 2] };
    let Some(rect) = melogold_core::frame_bars::content(&pixels) else { return Some(texture) };
    let mut cropped = Vec::with_capacity(rect.width * rect.height * 4);
    for y in rect.y..rect.y + rect.height {
        let start = y * stride + rect.x * 4;
        cropped.extend_from_slice(&data[start..start + rect.width * 4]);
    }
    let format =
        if cfg!(target_endian = "little") { gdk::MemoryFormat::B8g8r8a8Premultiplied } else { gdk::MemoryFormat::A8r8g8b8Premultiplied };
    Some(gdk::MemoryTexture::new(rect.width as i32, rect.height as i32, format, &glib::Bytes::from_owned(cropped), rect.width * 4).upcast())
}
