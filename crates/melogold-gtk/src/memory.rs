//! Память процесса: glibc malloc в многопоточном приложении.
//!
//! Замер 07.10.2026 (выпускная сборка, прогон съёмки по экранам с обложками, безэкранный weston):
//! пик личной памяти 232 МБ и до 101 потока. Потоки — пул `spawn_blocking` tokio: в нём идут
//! `tokio::fs` и разбор обложек, по потоку на картинку. glibc даёт потокам свои арены (до 8 на ядро),
//! и освобождённое в арене системе не возвращается. С двумя аренами тот же прогон — 172 МБ.
//! Поэтому: две арены, пул блокирующих потоков ограничен (`services.rs`), а освобождённое раз в
//! полминуты отдаётся системе (`malloc_trim`). Графика (драйверы GPU, текстуры) сюда не входит.

use gtk::glib;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    extern "C" {
        pub fn mallopt(param: i32, value: i32) -> i32;
        pub fn malloc_trim(pad: usize) -> i32;
    }

    /// `M_ARENA_MAX` из `<malloc.h>`.
    pub const M_ARENA_MAX: i32 = -8;
}

/// Арен malloc — две. До того как появились другие потоки; `MALLOC_ARENA_MAX` из окружения главнее.
pub fn limit_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        // SAFETY: mallopt лишь меняет настройку распределителя; вызывается до появления других потоков.
        unsafe {
            glibc::mallopt(glibc::M_ARENA_MAX, 2);
        }
    }
}

/// Раз в 30 с вернуть системе освобождённую память всех арен.
pub fn trim_periodically() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glib::timeout_add_seconds_local(30, || {
        // SAFETY: malloc_trim потокобезопасен, только освобождает свободные страницы.
        unsafe {
            glibc::malloc_trim(0);
        }
        glib::ControlFlow::Continue
    });
}
