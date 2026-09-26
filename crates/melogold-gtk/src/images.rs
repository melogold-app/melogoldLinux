//! Обложки (грабли §9 п. 6): до 320 px — `mqdefault`, а не 1280×720; грузятся параллельно и
//! кэшируются на диск (`$XDG_CACHE_HOME/melogold/images`) и в памяти (последние 300).

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
        let path = self.dir.join(format!("{}.img", hex::encode(&Sha256::digest(url.as_bytes())[..16])));
        let (http, fetch_url) = (self.http.clone(), url.clone());
        let bytes = self
            .runtime
            .spawn(async move {
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    return Some(bytes);
                }
                let mut response = http.get(&fetch_url).send().await.ok()?;
                if response.status() == 404 {
                    // У старых видео нет крупных кадров — есть только hqdefault (Windows `Thumbnails.Fallback`).
                    let fallback = melogold_core::thumbnails::fallback(&fetch_url)?;
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
            .ok()??;
        let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()?;
        self.remember(&url, &texture);
        Some(texture)
    }
}
