//! Правила Melogold без интерфейса (docs/PROMPT.md §3 «Устройство кода»).
//!
//! Здесь нет ни GTK, ни сети: всё, что можно проверить `cargo test` без экрана. Правила
//! повторяют `src/Melogold.Core` Windows и `:core:domain` Android, векторы — в `spec/`.

pub mod app_info;
pub mod artwork_colors;
pub mod countries;
mod countries_generated;
pub mod description;
pub mod devices;
pub mod frame_bars;
pub mod hwid;
pub mod ids;
pub mod import_ids;
pub mod iso;
pub mod links;
pub mod lyrics;
pub mod music;
pub mod music_services;
pub mod paths;
pub mod playlist_diff;
pub mod plurals;
pub mod pow;
pub mod queue;
pub mod search_top;
pub mod server_address;
pub mod settings;
pub mod share_links;
pub mod stats_window;
pub mod system;
pub mod text;
pub mod thumbnails;
pub mod title_cleaner;
pub mod updates;
pub mod youtube_links;
