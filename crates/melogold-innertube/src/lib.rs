//! Клиент YouTube Music и YouTube (InnerTube): поиск, подсказки, «Далее», поток, диагноз отказа.
//! Порт `src/Melogold.InnerTube` Windows и `providers/innertube` Android.

pub mod client;
pub mod json;
pub mod music;
mod music_parsers;
pub mod player;
mod web_parsers;

pub use client::{locale_from, ClientProfile, InnerTube};

/// Класс ошибки запроса к YouTube — по нему текст состояния экрана (REWRITE §3.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// Нет соединения с YouTube.
    Offline,
    /// Проверка на бота или блокировка.
    Blocked,
    /// YouTube изменил страницу.
    Parser,
    Unknown,
}

#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct YouTubeError {
    pub kind: ErrorKind,
    pub message: String,
}

impl YouTubeError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}
