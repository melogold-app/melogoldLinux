//! Журнал (docs/PROMPT.md §3 «Логи»): `logs/current.log` до 2 МБ, при старте прежний
//! становится `previous.log`; последнее падение — отдельным файлом `last-crash.txt`.
//! Пароли, токены и тела запросов сюда не пишутся.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

const MAX_BYTES: u64 = 2 * 1024 * 1024;

pub fn current_path(logs: &Path) -> PathBuf {
    logs.join("current.log")
}

pub fn previous_path(logs: &Path) -> PathBuf {
    logs.join("previous.log")
}

pub fn crash_path(logs: &Path) -> PathBuf {
    logs.join("last-crash.txt")
}

/// Поднимает журнал. Вызывается только в первом экземпляре: второй запуск лишь передаёт
/// аргументы и выходит, и если бы он тоже крутил файлы, журнал живого окна уехал бы в `previous`.
pub fn init(logs: &Path) {
    let file = LogFile::open(logs);
    // В отладочной сборке — debug: логи действий нужны ровно во время разработки, и
    // требовать для них RUST_LOG значит получать первый прогон без них (Clementine).
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(if cfg!(debug_assertions) { "warn,melogold=debug" } else { "warn,melogold=info" }))
    };
    let to_file = fmt::layer().with_ansi(false).with_thread_names(true).with_writer(move || file.clone());
    let to_stderr = fmt::layer().with_writer(io::stderr);
    let _ = tracing_subscriber::registry().with(to_file.with_filter(filter())).with(to_stderr.with_filter(filter())).try_init();

    let crash = crash_path(logs);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("падение: {info}");
        let _ = std::fs::write(&crash, format!("Melogold {}\n{}\n\n{info}\n\n{backtrace}\n", melogold_core::app_info::VERSION, now_utc()));
        default_hook(info);
    }));
}

/// Файл журнала, который сам себя обрезает: больше 2 МБ — в `previous.log` и заново.
#[derive(Clone)]
struct LogFile(Arc<Mutex<Inner>>);

struct Inner {
    dir: PathBuf,
    file: Option<File>,
    written: u64,
}

impl LogFile {
    fn open(dir: &Path) -> Self {
        let _ = std::fs::create_dir_all(dir);
        let current = current_path(dir);
        if current.exists() {
            let _ = std::fs::rename(&current, previous_path(dir));
        }
        Self(Arc::new(Mutex::new(Inner { dir: dir.to_owned(), file: File::create(current).ok(), written: 0 })))
    }
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Ok(mut inner) = self.0.lock() else { return Ok(buf.len()) };
        // Режем до записи: файл не выходит за 2 МБ, строка целиком уходит в новый.
        if inner.written > 0 && inner.written + buf.len() as u64 > MAX_BYTES {
            let current = current_path(&inner.dir);
            inner.file = None;
            let _ = std::fs::rename(&current, previous_path(&inner.dir));
            inner.file = File::create(current).ok();
            inner.written = 0;
        }
        if let Some(file) = inner.file.as_mut() {
            // Сбой записи журнала не должен ронять приложение: строку теряем, работаем дальше.
            if file.write_all(buf).is_ok() {
                inner.written += buf.len() as u64;
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Ok(mut inner) = self.0.lock() {
            if let Some(file) = inner.file.as_mut() {
                let _ = file.flush();
            }
        }
        Ok(())
    }
}

fn now_utc() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix {seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_at_start_and_past_the_limit() {
        let dir = std::env::temp_dir().join(format!("melogold-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(current_path(&dir), "прошлый запуск").unwrap();

        let mut log = LogFile::open(&dir);
        assert_eq!(std::fs::read_to_string(previous_path(&dir)).unwrap(), "прошлый запуск");
        let line = vec![b'x'; 1024 * 1024];
        for _ in 0..3 {
            log.write_all(&line).unwrap();
        }
        log.flush().unwrap();
        let current = std::fs::metadata(current_path(&dir)).unwrap().len();
        assert!(current <= MAX_BYTES, "current.log {current} байт");
        assert_eq!(std::fs::metadata(previous_path(&dir)).unwrap().len(), MAX_BYTES);
        let _ = std::fs::remove_dir_all(dir);
    }
}
