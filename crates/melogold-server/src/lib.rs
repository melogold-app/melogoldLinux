//! Сервер Melogold (docs/PROMPT.md §3 «Устройство кода»): API, аккаунт и токены, синхронизация
//! библиотеки и истории, живые события. Контракт — `melogoldServer/docs/API.md`.

pub mod account;
pub mod api;
pub mod dto;
pub mod session;
pub mod sync;
