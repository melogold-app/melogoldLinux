//! Язык интерфейса: русский и английский (docs/PROMPT.md §3 «Строки»).
//!
//! Строки — из общего каталога Windows (`strings_generated.rs`, скрипт
//! `scripts/sync-strings.py`), а не пишутся заново: две разные подписи под одной
//! кнопкой человек с двумя устройствами замечает сразу. Чего у Windows нет —
//! в [`LINUX_ONLY`]; когда то же понятие появится у других клиентов, строку надо
//! перенести в общий каталог и удалить отсюда.

use std::fmt::Display;
use std::sync::OnceLock;

use melogold_core::plurals;

use crate::strings_generated::STRINGS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Ru,
    En,
}

impl Lang {
    pub fn parse(code: &str) -> Option<Lang> {
        let code = code.trim().to_ascii_lowercase();
        if code.starts_with("ru") {
            Some(Lang::Ru)
        } else if code.starts_with("en") {
            Some(Lang::En)
        } else {
            None
        }
    }
}

/// (ключ, русский, английский). Ключи — с префиксом `Linux`, чтобы не спутать с общими.
const LINUX_ONLY: &[(&str, &str, &str)] = &[
    ("LinuxMainMenu", "Главное меню", "Main Menu"),
    ("LinuxAppLanguage", "Язык приложения", "App language"),
    ("LinuxLanguageRestart", "Язык сменится после перезапуска Melogold", "The language changes after Melogold restarts"),
    ("LinuxAbout", "О Melogold", "About Melogold"),
    (
        "LinuxAboutComments",
        "Музыка из YouTube Music и YouTube с общей библиотекой на всех устройствах",
        "Music from YouTube Music and YouTube with one library on all your devices",
    ),
    ("LinuxShowSidebar", "Показать разделы", "Show Sections"),
    // У Windows «F1 или Ctrl+/»; окно сочетаний в GNOME открывается по Ctrl+? (HIG).
    ("LinuxShortcutsDescription", "Все клавиши приложения · F1 или Ctrl+?", "All keys of the app · F1 or Ctrl+?"),
    ("LinuxCloseWindow", "Закрыть окно", "Close window"),
    ("LinuxQuit", "Выйти", "Quit"),
    ("LinuxPrimaryMenuShortcut", "Главное меню", "Main menu"),
    ("LinuxVersions", "Версии", "Versions"),
    ("LinuxSystem", "Система", "System"),
    ("LinuxLogs", "Журналы", "Logs"),
    ("LinuxLogsFolder", "Папка журналов", "Logs folder"),
    ("LinuxSectionSoonTrends", "В тренде и настроения появятся в следующем срезе", "Trending and moods are coming next"),
    ("LinuxSectionSoonNew", "Новые релизы и «Для вас» появятся позже", "New releases and For you are coming later"),
    ("LinuxSectionSoonLibrary", "Избранное, плейлисты и история появятся позже", "Favorites, playlists and history are coming later"),
    ("LinuxServerLinkOpened", "Ссылка на сервер: {0}. Вход появится вместе с аккаунтом", "Server link: {0}. Sign-in comes with accounts"),
    (
        "LinuxLinkRequest",
        "Откройте Melogold на другом устройстве → Добавить устройство → Сканировать",
        "Open Melogold on the other device → Add a device → Scan",
    ),
];

static LANG: OnceLock<Lang> = OnceLock::new();

/// Язык процесса: выбранный в «Язык приложения» (или язык снимков), иначе язык системы.
///
/// Вызывается до GTK. Выбранный язык ставится и в `LANGUAGE`: свои подписи GTK и
/// libadwaita («Search shortcuts», «Close») берутся через gettext, и без этого окно
/// выходило бы на двух языках сразу.
pub fn init(forced: Option<Lang>) -> Lang {
    if let Some(lang) = forced {
        // Один поток, GTK ещё не запущен: переменная окружения меняется безопасно.
        std::env::set_var("LANGUAGE", if lang == Lang::Ru { "ru" } else { "en" });
    }
    *LANG.get_or_init(|| forced.unwrap_or_else(detect))
}

pub fn lang() -> Lang {
    *LANG.get_or_init(detect)
}

pub fn is_russian() -> bool {
    lang() == Lang::Ru
}

/// Порядок как у gettext: `LANGUAGE` (первый из списка), затем `LC_ALL`, `LC_MESSAGES`, `LANG`.
fn detect() -> Lang {
    if let Some(lang) = std::env::var("MELOGOLD_LANG").ok().as_deref().and_then(Lang::parse) {
        return lang;
    }
    for name in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(name) else { continue };
        let first = value.split(':').next().unwrap_or_default().trim();
        if first.is_empty() || first == "C" || first == "POSIX" || first.starts_with("C.") {
            continue;
        }
        return Lang::parse(first).unwrap_or(Lang::En);
    }
    Lang::En
}

fn lookup(key: &str) -> Option<&'static str> {
    let pick = |ru: &'static str, en: &'static str| if is_russian() { ru } else { en };
    if let Some((_, ru, en)) = LINUX_ONLY.iter().find(|(k, ..)| *k == key) {
        return Some(pick(ru, en));
    }
    STRINGS.binary_search_by(|(k, ..)| (*k).cmp(key)).ok().map(|index| {
        let (_, ru, en) = STRINGS[index];
        pick(ru, en)
    })
}

/// Строка по ключу; нет такой — сам ключ (и тест `every_key_in_code_exists` это ловит).
pub fn tr(key: &'static str) -> &'static str {
    lookup(key).unwrap_or(key)
}

/// Строка с подстановками `{0}`, `{1}`… как у Windows (`string.Format`).
pub fn trf(key: &'static str, args: &[&dyn Display]) -> String {
    format_args_into(tr(key), args)
}

fn format_args_into(template: &str, args: &[&dyn Display]) -> String {
    let mut text = template.to_owned();
    for (index, arg) in args.iter().enumerate() {
        text = text.replace(&format!("{{{index}}}"), &arg.to_string());
    }
    text
}

/// «21 трек», «3 трека», «5 треков»: ключ с суффиксом формы, `{0}` — число.
#[allow(dead_code)] // первые счётчики — в срезе 2
pub fn plural(key: &'static str, count: i64) -> String {
    let form = plurals::form(count, is_russian());
    let full = format!("{key}_{}", form.suffix());
    let template = lookup(&full).unwrap_or(key);
    format_args_into(template, &[&format_count(count)])
}

/// Число с разделителем разрядов: «12 345» (неразрывный пробел) и «12,345».
#[allow(dead_code)]
pub fn format_count(count: i64) -> String {
    let digits = count.unsigned_abs().to_string();
    let separator = if is_russian() { '\u{a0}' } else { ',' };
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(separator);
        }
        out.push(digit);
    }
    if count < 0 {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_for_binary_search() {
        assert!(STRINGS.windows(2).all(|pair| pair[0].0 < pair[1].0));
    }

    #[test]
    fn linux_only_keys_do_not_shadow_shared_ones() {
        for (key, ..) in LINUX_ONLY {
            assert!(key.starts_with("Linux"), "{key}");
            assert!(STRINGS.binary_search_by(|(k, ..)| (*k).cmp(key)).is_err(), "{key} уже есть в общем каталоге");
        }
    }

    /// Каждый ключ `tr("…")`, `trf("…")`, `plural("…")` в исходниках есть в таблицах.
    #[test]
    fn every_key_in_code_exists() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missing = Vec::new();
        visit(&src, &mut |text| {
            for call in ["tr(\"", "trf(\"", "plural(\""] {
                for (start, _) in text.match_indices(call) {
                    let rest = &text[start + call.len()..];
                    let Some(end) = rest.find('"') else { continue };
                    let key = &rest[..end];
                    // Упоминания в комментариях («`tr("…")`») — не вызовы.
                    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
                        continue;
                    }
                    let exists = if call == "plural(\"" { lookup(&format!("{key}_one")).is_some() } else { lookup(key).is_some() };
                    if !exists {
                        missing.push(key.to_owned());
                    }
                }
            }
        });
        assert!(missing.is_empty(), "нет строк: {missing:?}");
    }

    fn visit(dir: &std::path::Path, check: &mut dyn FnMut(&str)) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, check);
            } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("strings_generated.rs") {
                check(&std::fs::read_to_string(&path).unwrap());
            }
        }
    }

    #[test]
    fn format_helpers() {
        assert_eq!(format_args_into("{0} · {1}", &[&"a", &2]), "a · 2");
        let _ = init(Some(Lang::Ru));
        assert_eq!(format_count(1234567), "1\u{a0}234\u{a0}567");
        assert_eq!(format_count(12), "12");
    }
}
