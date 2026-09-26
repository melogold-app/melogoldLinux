//! Воспроизведение (docs/PROMPT.md §4): поток без yt-dlp, байты из загрузок, кэша или сети
//! короткими диапазонами, плеер на GStreamer.

pub mod engine;
pub mod fmp4;
pub mod output;
pub mod reader;
pub mod resolver;
pub mod song_cache;
pub mod stream;
pub mod stream_clients;
