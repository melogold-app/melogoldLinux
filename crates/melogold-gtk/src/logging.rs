//! Журнал (docs/PROMPT.md §3 «Логи», задание 0037): каждый запуск пишет в свой
//! `logs/melogold-ГГГГММДД-ЧЧММСС.log`, новый файл — после 2 МБ или со сменой даты; падение —
//! `last-crash.txt`. Сколько хранить (день, 3 дня, неделя, месяц) и писать ли вообще — в «Диагностике»;
//! сверх срока вся папка не больше 50 МБ. Выключили — запись встаёт и папка пустеет сразу.
//! Пароли, токены и тела запросов сюда не пишутся.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use gtk::glib;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

const MAX_BYTES: u64 = 2 * 1024 * 1024;
/// Потолок всей папки: месяц журнала у болтливого запуска не должен съесть диск.
const MAX_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const CRASH_FILE: &str = "last-crash.txt";

/// Сроки хранения в днях — «День», «3 дня», «Неделя», «Месяц».
pub const RETENTION_DAYS: [i64; 4] = [1, 3, 7, 30];

static LOG: OnceLock<LogFile> = OnceLock::new();

pub fn crash_path(logs: &Path) -> PathBuf {
    logs.join(CRASH_FILE)
}

/// Поднимает журнал. Вызывается только в первом экземпляре: второй запуск лишь передаёт аргументы и
/// выходит — его уборка могла бы удалить файл живого окна.
pub fn init(logs: &Path, enabled: bool, retention_days: i64) {
    let file = LOG.get_or_init(|| LogFile::open(logs, enabled, retention_days)).clone();
    // В отладочной сборке — debug: логи действий нужны ровно во время разработки, и
    // требовать для них RUST_LOG значит получать первый прогон без них (Clementine).
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(if cfg!(debug_assertions) { "warn,melogold=debug" } else { "warn,melogold=info" }))
    };
    let to_file = fmt::layer().with_ansi(false).with_thread_names(true).with_writer(move || file.clone());
    // stderr приложения, запущенного из меню, GNOME кладёт в системный журнал: выключенный журнал молчит и там.
    let to_stderr = fmt::layer().with_writer(|| -> Box<dyn Write> {
        if enabled_now() || cfg!(debug_assertions) {
            Box::new(io::stderr())
        } else {
            Box::new(io::sink())
        }
    });
    let _ = tracing_subscriber::registry().with(to_file.with_filter(filter())).with(to_stderr.with_filter(filter())).try_init();

    let crash = crash_path(logs);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if enabled_now() {
            let backtrace = std::backtrace::Backtrace::force_capture();
            tracing::error!("падение: {info}");
            let _ =
                std::fs::write(&crash, format!("Melogold {}\n{}\n\n{info}\n\n{backtrace}\n", melogold_core::app_info::VERSION, now_utc()));
        }
        default_hook(info);
    }));
}

fn enabled_now() -> bool {
    LOG.get().is_some_and(|log| log.0.lock().is_ok_and(|inner| inner.enabled))
}

/// «Журнал» в «Диагностике»: выключили — файл закрыт, папка пуста; включили — сразу новый файл.
pub fn set_enabled(enabled: bool) {
    let Some(log) = LOG.get() else { return };
    let Ok(mut inner) = log.0.lock() else { return };
    if inner.enabled == enabled {
        return;
    }
    inner.enabled = enabled;
    inner.file = None;
    inner.path = None;
    if !enabled {
        remove_all(&inner.dir);
    }
}

/// «Хранить»: новый срок действует сразу — лишнее удаляется.
pub fn set_retention(days: i64) {
    let Some(log) = LOG.get() else { return };
    let Ok(mut inner) = log.0.lock() else { return };
    inner.keep = keep_for(days);
    cleanup(&inner.dir, SystemTime::now(), inner.keep, inner.path.as_deref());
}

fn keep_for(days: i64) -> Duration {
    DAY * u32::try_from(days.clamp(1, 365)).unwrap_or(7)
}

/// Файл журнала: пишет в файл этого запуска и сам начинает новый после 2 МБ или в полночь.
#[derive(Clone)]
struct LogFile(Arc<Mutex<Inner>>);

struct Inner {
    dir: PathBuf,
    enabled: bool,
    keep: Duration,
    file: Option<File>,
    path: Option<PathBuf>,
    written: u64,
    /// Полночь после открытия файла: с новой датой — новый файл.
    rollover: SystemTime,
}

impl LogFile {
    fn open(dir: &Path, enabled: bool, retention_days: i64) -> Self {
        let _ = std::fs::create_dir_all(dir);
        let inner = Inner {
            dir: dir.to_owned(),
            enabled,
            keep: keep_for(retention_days),
            file: None,
            path: None,
            written: 0,
            rollover: SystemTime::now(),
        };
        if enabled {
            cleanup(dir, SystemTime::now(), inner.keep, None);
        } else {
            remove_all(dir);
        }
        Self(Arc::new(Mutex::new(inner)))
    }
}

impl Inner {
    /// Новый файл с местным временем в имени; заодно — уборка по сроку и месту.
    fn start_file(&mut self) {
        let now = glib::DateTime::now_local().ok();
        let stamp = now.as_ref().and_then(|t| t.format("%Y%m%d-%H%M%S").ok()).map(|s| s.to_string()).unwrap_or_else(now_utc);
        let path = self.dir.join(format!("melogold-{stamp}.log"));
        let _ = std::fs::create_dir_all(&self.dir);
        self.file = File::options().create(true).append(true).open(&path).ok();
        self.written = self.file.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        self.rollover = now.as_ref().and_then(next_midnight).unwrap_or_else(|| SystemTime::now() + DAY);
        cleanup(&self.dir, SystemTime::now(), self.keep, Some(path.as_path()));
        self.path = Some(path);
    }
}

fn next_midnight(now: &glib::DateTime) -> Option<SystemTime> {
    let tomorrow = now.add_days(1).ok()?;
    let midnight = glib::DateTime::from_local(tomorrow.year(), tomorrow.month(), tomorrow.day_of_month(), 0, 0, 0.0).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(midnight.to_unix()).ok()?))
}

impl Write for LogFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Ok(mut inner) = self.0.lock() else { return Ok(buf.len()) };
        if !inner.enabled {
            return Ok(buf.len());
        }
        // Режем до записи: файл не выходит за 2 МБ, строка целиком уходит в новый.
        let full = inner.written > 0 && inner.written + buf.len() as u64 > MAX_BYTES;
        if inner.file.is_none() || full || SystemTime::now() >= inner.rollover {
            inner.start_file();
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

/// Файлы журнала в папке — `*.log` и падение — от старых к новым по времени изменения, с размером.
fn files(dir: &Path) -> Vec<(PathBuf, SystemTime, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "log") || path.file_name().is_some_and(|n| n == CRASH_FILE))
        .filter_map(|path| {
            let meta = std::fs::metadata(&path).ok()?;
            meta.is_file().then(|| (path, meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), meta.len()))
        })
        .collect();
    files.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    files
}

/// Уборка: старше срока — удалить; всё вместе больше 50 МБ — удалять самые старые. Файл, в который
/// идёт запись, не трогаем. Старые `current.log` и `previous.log` (до 0037) — тоже `*.log`.
fn cleanup(dir: &Path, now: SystemTime, keep: Duration, writing: Option<&Path>) {
    let mut kept = Vec::new();
    for (path, modified, size) in files(dir) {
        let old = now.duration_since(modified).is_ok_and(|age| age > keep);
        if old && Some(path.as_path()) != writing {
            let _ = std::fs::remove_file(&path);
        } else {
            kept.push((path, size));
        }
    }
    let mut total: u64 = kept.iter().map(|(_, size)| size).sum();
    for (path, size) in kept {
        if total <= MAX_TOTAL_BYTES {
            break;
        }
        if Some(path.as_path()) != writing && std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

fn remove_all(dir: &Path) {
    for (path, ..) in files(dir) {
        let _ = std::fs::remove_file(path);
    }
}

/// Журнал одним файлом, чтобы отправить (31.10.2026, Максим: «чтобы логи уходили файлом»): шапка,
/// последнее падение, затем все файлы от старых к новым. Байты не по UTF-8 (строка оборвалась на
/// падении) не мешают остальному.
pub fn export(logs: &Path, header: &str) -> String {
    let mut text = header.trim_end().to_owned();
    text.push('\n');
    let all = files(logs);
    let crash = all.iter().filter(|(path, ..)| path.file_name().is_some_and(|n| n == CRASH_FILE));
    let logs = all.iter().filter(|(path, ..)| path.file_name().is_none_or(|n| n != CRASH_FILE));
    for (path, ..) in crash.chain(logs) {
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = if name == CRASH_FILE { format!("Падение ({name})") } else { name };
        text.push_str(&format!("\n===== {title} =====\n"));
        text.push_str(&String::from_utf8_lossy(&bytes));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    text
}

fn now_utc() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix {seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("melogold-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn aged(dir: &Path, name: &str, content: &[u8], age: Duration) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        File::options().write(true).open(&path).unwrap().set_modified(SystemTime::now() - age).unwrap();
        path
    }

    #[test]
    fn a_new_file_past_the_limit_and_off_empties_the_folder() {
        let dir = temp_dir("logs");
        let mut log = LogFile::open(&dir, true, 7);
        let line = vec![b'x'; 1024 * 1024];
        for _ in 0..3 {
            log.write_all(&line).unwrap();
            // Имя файла — с точностью до секунды: второй файл в ту же секунду дописывался бы в первый.
            std::thread::sleep(Duration::from_millis(1100));
        }
        log.flush().unwrap();
        let written = files(&dir);
        assert_eq!(written.len(), 2, "{written:?}");
        assert!(written.iter().all(|(.., size)| *size <= MAX_BYTES));

        log.0.lock().unwrap().enabled = false;
        remove_all(&dir);
        log.write_all(b"after").unwrap();
        assert!(files(&dir).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cleanup_drops_what_is_older_than_the_term_then_the_oldest_past_50_mb() {
        let dir = temp_dir("cleanup");
        let old = aged(&dir, "melogold-20261001-100000.log", b"x", DAY * 8);
        let crash = aged(&dir, CRASH_FILE, b"x", DAY * 8);
        let legacy = aged(&dir, "previous.log", b"x", HOUR * 3);
        let other = aged(&dir, "settings.json", b"x", DAY * 100);
        cleanup(&dir, SystemTime::now(), DAY * 7, None);
        assert!(!old.exists() && !crash.exists(), "старше недели");
        assert!(legacy.exists() && other.exists(), "свежий журнал и чужой файл остаются");

        let big = vec![b'x'; 30 * 1024 * 1024];
        let earlier = aged(&dir, "melogold-20261030-100000.log", &big, HOUR * 2);
        let writing = aged(&dir, "melogold-20261030-110000.log", &big, HOUR);
        cleanup(&dir, SystemTime::now(), DAY * 7, Some(&writing));
        assert!(!legacy.exists() && !earlier.exists(), "больше 50 МБ — самые старые уходят первыми");
        assert!(writing.exists(), "в него идёт запись");
        assert!(other.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn export_is_the_header_then_the_crash_then_files_from_old_to_new() {
        let dir = temp_dir("export");
        aged(&dir, "melogold-20261031-120000.log", "этот".as_bytes(), Duration::ZERO);
        aged(&dir, "melogold-20261030-120000.log", b"\xff\xfe\n", HOUR);

        let text = export(&dir, "Melogold 1.0\n");
        assert!(text.starts_with("Melogold 1.0\n"));
        assert!(!text.contains("Падение"), "нет last-crash.txt — нет и раздела");
        let before = text.find("===== melogold-20261030-120000.log =====").unwrap();
        let now = text.find("===== melogold-20261031-120000.log =====\nэтот\n").unwrap();
        assert!(before < now);

        aged(&dir, CRASH_FILE, b"panic", HOUR * 5);
        let text = export(&dir, "Melogold 1.0");
        let crash = text.find("===== Падение (last-crash.txt) =====\npanic\n").unwrap();
        assert!(crash < text.find("===== melogold-20261030").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
