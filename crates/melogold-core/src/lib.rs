//! Правила Melogold без интерфейса (docs/PROMPT.md §3 «Устройство кода»).
//!
//! Здесь нет ни GTK, ни сети: всё, что можно проверить `cargo test` без экрана. Правила
//! повторяют `src/Melogold.Core` Windows и `:core:domain` Android, векторы — в `spec/`.

pub mod app_info;
pub mod countries;
mod countries_generated;
pub mod links;
pub mod music;
pub mod paths;
pub mod plurals;
pub mod queue;
pub mod settings;
pub mod system;
pub mod text;
pub mod thumbnails;
