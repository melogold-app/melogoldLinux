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
    ("LinuxCheckExtraction", "Проверить извлечение", "Check extraction"),
    (
        "LinuxCheckExtractionHint",
        "Получает поток тестового трека: время, формат и клиент",
        "Gets a test track stream: time, format and client",
    ),
    ("LinuxCheck", "Проверить", "Check"),
    ("LinuxSkip", "Пропустить", "Skip"),
    ("LinuxSectionSoonTrends", "В тренде и настроения появятся в следующем срезе", "Trending and moods are coming next"),
    ("LinuxSectionSoonNew", "Новые релизы и «Для вас» появятся позже", "New releases and For you are coming later"),
    // Где лежат токены входа (docs/PROMPT.md §3 «Где что лежит»).
    ("LinuxTokens", "Токены входа", "Sign-in tokens"),
    ("LinuxTokensInKeyring", "В связке ключей", "In the keyring"),
    (
        "LinuxTokensInFile",
        "Связки ключей нет: в файле, который может читать только ваш пользователь",
        "No keyring: in a file only your user can read",
    ),
    // Свои названия трека (задание 0005): у Android есть, у Windows пока нет.
    ("LinuxEditDetails", "Изменить сведения…", "Edit details…"),
    ("LinuxTrackDetails", "Сведения о треке", "Track details"),
    ("LinuxTrackDetailsName", "Название", "Title"),
    ("LinuxTrackDetailsArtist", "Исполнитель", "Artist"),
    ("LinuxTrackDetailsAlbum", "Альбом", "Album"),
    ("LinuxTrackDetailsReset", "Как на YouTube", "As on YouTube"),
    ("LinuxTrackDetailsHint", "Пустое поле — как на YouTube", "An empty field is as on YouTube"),
    ("LinuxSetAlbum", "Указать альбом…", "Set album…"),
    ("LinuxSetAlbumTitle", "Указать альбом", "Set album"),
    ("LinuxAlbumSetFormat", "Альбом указан: {0}", "Album set: {0}"),
    // Метка строки: трек целиком в кэше музыки и играет без сети (у Windows только раздел «В кэше · …»).
    ("LinuxInCache", "В кэше — играет без сети", "Cached — plays offline"),
    ("LinuxLyricsEditorNotSynced", "Отметьте строки на вкладке «Синхронизация»", "Mark the lines on the Sync tab"),
    (
        "LinuxInviteLater",
        "Вход по приглашению другого устройства появится в следующей версии",
        "Signing in with another device's invitation comes in a later version",
    ),
    (
        "LinuxServerIdMismatch",
        "По этому адресу — другой сервер, не тот, что в ссылке",
        "This address leads to a different server than the link",
    ),
    // Редактор текста, «Синхронизация» (задание 0007): подпись над следующей строкой целиком.
    ("LinuxLyricsEditorNext", "Далее", "Next"),
    (
        "LinuxLinkRequest",
        "Откройте Melogold на другом устройстве → Добавить устройство → Сканировать",
        "Open Melogold on the other device → Add a device → Scan",
    ),
    // ── задание 0008: вход по коду ──
    ("LinuxLinkSignInByCode", "Войти по коду", "Sign in with a code"),
    ("LinuxLinkCodeTitle", "Вход по коду", "Sign in with a code"),
    (
        "LinuxLinkCodeText",
        "На устройстве, где вы уже вошли, откройте Аккаунт › Добавить устройство и введите этот код",
        "On a device where you're signed in, open Account › Add device and enter this code",
    ),
    ("LinuxLinkValidForFormat", "Действует {0}", "Valid for {0}"),
    ("LinuxLinkReconnecting", "Нет связи с сервером. Пробуем снова…", "Can't reach the server. Trying again…"),
    ("LinuxLinkHaveCode", "У меня есть код с другого устройства", "I have a code from another device"),
    ("LinuxLinkShowMyCode", "Показать мой код", "Show my code"),
    (
        "LinuxLinkClaimText",
        "Введите код, который показывает другое устройство: Аккаунт › Добавить устройство › Показать код для нового устройства",
        "Enter the code another device shows: Account › Add device › Show a code for a new device",
    ),
    ("LinuxLinkVerifyPickFormat", "Выберите это число на «{0}»", "Choose this number on “{0}”"),
    ("LinuxLinkVerifyAccountFormat", "Вход в аккаунт {0}", "Signing in to {0}"),
    ("LinuxLinkNumberFormat", "Число {0}", "Number {0}"),
    ("LinuxLinkCodeSpokenFormat", "Код для входа: {0}", "Sign-in code: {0}"),
    ("LinuxLinkStarting", "Получаем код…", "Getting a code…"),
    ("LinuxLinkSigningIn", "Входим…", "Signing in…"),
    ("LinuxLinkNewCode", "Получить новый код", "Get a new code"),
    ("LinuxLinkEnterOtherCode", "Ввести другой код", "Enter another code"),
    ("LinuxLinkBackToPassword", "Войти по паролю", "Sign in with a password"),
    ("LinuxLinkErrorDenied", "Вход отклонён на другом устройстве", "Sign-in was denied on the other device"),
    ("LinuxLinkErrorExpired", "Код устарел. Получите новый", "The code has expired. Get a new one"),
    (
        "LinuxLinkErrorClaimExpired",
        "Код устарел. Покажите новый на устройстве, где вы уже вошли",
        "The code has expired. Show a new one on the device where you're signed in",
    ),
    ("LinuxLinkErrorCancelled", "Вход отменили на другом устройстве", "Sign-in was canceled on the other device"),
    (
        "LinuxLinkErrorWrongMode",
        "Это код нового устройства. Введите его там, где вы уже вошли: Аккаунт › Добавить устройство",
        "This is a new device's code. Enter it where you're signed in: Account › Add device",
    ),
    ("LinuxLinkShowCode", "Показать код для нового устройства", "Show a code for a new device"),
    (
        "LinuxLinkInviteText",
        "На новом устройстве выберите «Войти по коду» и «У\u{a0}меня есть код с другого устройства», затем введите этот код",
        "On the new device, choose “Sign in with a code” and “I have a code from another device”, then enter this code",
    ),
    ("LinuxLinkWaitingNew", "Ждём новое устройство…", "Waiting for the new device…"),
    ("LinuxLinkInviteExpired", "Код устарел. Покажите новый", "The code has expired. Show a new one"),
    ("LinuxLinkInviteCancelled", "Приглашение отменено", "The invitation was canceled"),
    // ── задание 0010: ссылки ──
    ("LinuxCopyLink", "Скопировать ссылку", "Copy link"),
    ("LinuxMyLinks", "Мои ссылки", "My links"),
    (
        "LinuxMyLinksText",
        "Плейлисты, которыми вы поделились. Удалённая ссылка перестаёт открываться",
        "Playlists you shared. A deleted link stops opening",
    ),
    ("LinuxMyLinksEmpty", "Ссылок пока нет", "No links yet"),
    (
        "LinuxMyLinksEmptyText",
        "Откройте меню своего плейлиста и выберите «Скопировать ссылку»",
        "Open the menu of your playlist and choose “Copy link”",
    ),
    ("LinuxDeleteLink", "Удалить ссылку", "Delete link"),
    ("LinuxLinkDeleted", "Ссылка удалена", "Link deleted"),
    ("LinuxSharedPlaylist", "Плейлист по ссылке", "Playlist from a link"),
    ("LinuxSaveToLibrary", "Сохранить в Библиотеку", "Save to Library"),
    ("LinuxSavedToLibrary", "Сохранено в Библиотеку", "Saved to Library"),
    ("LinuxShareGone", "Ссылка удалена или неверна", "The link was deleted or is invalid"),
    ("LinuxShareFirst50", "Ссылка откроет первые 50 треков на YouTube", "The link opens the first 50 tracks on YouTube"),
    (
        "LinuxShareLimit",
        "Слишком много ссылок. Удалите ненужные: Аккаунт › Мои ссылки",
        "Too many links. Delete the ones you don't need: Account › My links",
    ),
    ("LinuxShareEmpty", "В плейлисте пока нет треков", "This playlist has no tracks yet"),
    (
        "LinuxImportPlaylistsLater",
        "Импорт плейлистов из других сервисов появится позже",
        "Importing playlists from other services is coming later",
    ),
    ("LinuxLinkOpening", "Открываем ссылку…", "Opening the link…"),
    ("LinuxLinkKindTrack", "трек", "track"),
    ("LinuxLinkKindArtist", "исполнитель", "artist"),
    // ── задание 0011: управление другим устройством ──
    ("LinuxRemoteDevice", "Устройство", "Device"),
    ("LinuxRemoteThisDevice", "Это устройство", "This device"),
    ("LinuxRemoteOthers", "Другие устройства", "Other devices"),
    ("LinuxRemoteOnline", "В сети", "Online"),
    ("LinuxRemoteOffline", "Не в сети", "Offline"),
    ("LinuxRemoteControlOff", "Управление выключено", "Control is off"),
    ("LinuxRemotePlayingOnFormat", "Играет на «{0}»", "Playing on “{0}”"),
    ("LinuxRemoteListenHere", "Слушать здесь", "Listen here"),
    ("LinuxRemoteDisconnect", "Отключиться", "Disconnect"),
    ("LinuxRemoteVolume", "Громкость", "Volume"),
    ("LinuxRemoteAllow", "Управление с других устройств", "Control from other devices"),
    (
        "LinuxRemoteAllowText",
        "Другие устройства аккаунта смогут ставить музыку на паузу, перематывать и менять громкость",
        "Other devices of your account can pause, seek and change the volume",
    ),
    ("LinuxRemoteControlledByFormat", "Управляет «{0}»", "“{0}” is in control"),
    ("LinuxRemoteOfflineFormat", "„{0}“ не в сети", "“{0}” is offline"),
    ("LinuxRemoteDisabledFormat", "На „{0}“ управление выключено", "Control is off on “{0}”"),
    ("LinuxRemoteNothingPlaying", "Ничего не играет", "Nothing is playing"),
    ("LinuxRemoteNoDevices", "Других устройств пока нет", "No other devices yet"),
    ("LinuxRemoteNothingToTake", "На устройстве нечего забрать", "There is nothing to take from the device"),
    // ── задание 0009: Итоги (тексты — Android GLOSSARY §4.16) ──
    ("LinuxStats", "Итоги", "Insights"),
    ("LinuxStatsWeek", "Неделя", "Week"),
    ("LinuxStatsMonth", "Месяц", "Month"),
    ("LinuxStatsPrevious", "Предыдущий период", "Previous period"),
    ("LinuxStatsNext", "Следующий период", "Next period"),
    ("LinuxStatsListeningTime", "Время прослушивания", "Listening time"),
    ("LinuxStatsPlaysLabel", "Прослушиваний", "Plays"),
    ("LinuxStatsTracksLabel", "Треков", "Tracks"),
    ("LinuxStatsArtistsLabel", "Исполнителей", "Artists"),
    ("LinuxStatsTopTracks", "Лучшие треки", "Top tracks"),
    ("LinuxStatsTopArtists", "Лучшие исполнители", "Top artists"),
    ("LinuxStatsTopAlbums", "Лучшие альбомы", "Top albums"),
    ("LinuxStatsWhen", "Когда вы слушали", "When you listened"),
    ("LinuxStatsTimeOfDay", "Время суток", "Time of day"),
    ("LinuxStatsDiscoveries", "Открытия", "Discoveries"),
    ("LinuxStatsDiscoveriesText", "Услышаны впервые в этом периоде", "Heard for the first time in this period"),
    ("LinuxStatsShowAll", "Показать все", "Show all"),
    ("LinuxStatsShowLess", "Свернуть", "Show less"),
    ("LinuxStatsEmpty", "За этот период прослушиваний нет", "No plays in this period"),
    (
        "LinuxStatsAllTimeNote",
        "Сервер хранит историю 400 дней: на новом устройстве видно столько, сколько пришло с сервера",
        "The server keeps the history for 400 days: on a new device you see as much as came from the server",
    ),
    ("LinuxStatsPlays_one", "{0} прослушивание", "{0} play"),
    ("LinuxStatsPlays_few", "{0} прослушивания", "{0} plays"),
    ("LinuxStatsPlays_many", "{0} прослушиваний", "{0} plays"),
    ("LinuxStatsPlays_other", "{0} прослушивания", "{0} plays"),
    ("LinuxStatsNew_one", "{0} новый трек", "{0} new track"),
    ("LinuxStatsNew_few", "{0} новых трека", "{0} new tracks"),
    ("LinuxStatsNew_many", "{0} новых треков", "{0} new tracks"),
    ("LinuxStatsNew_other", "{0} нового трека", "{0} new tracks"),
    ("LinuxStatsChange", "{0} к {1}", "{0} vs {1}"),
    ("LinuxStatsVsLastWeek", "прошлой неделе", "last week"),
    ("LinuxStatsVsYear", "{0} году", "{0}"),
    ("LinuxStatsBusiest", "Больше всего слушали: {0}, {1}", "Most listening: {0}, {1}"),
    ("LinuxStatsHoursBusiest", "Больше всего слушали около {0}, {1}", "Most listening around {0}, {1}"),
    ("LinuxDayNight", "Ночь", "Night"),
    ("LinuxDayMorning", "Утро", "Morning"),
    ("LinuxDayAfternoon", "День", "Afternoon"),
    ("LinuxDayEvening", "Вечер", "Evening"),
    ("LinuxWrapped", "Итоги года", "Year in review"),
    ("LinuxWrappedTitle", "Итоги {0}", "Insights {0}"),
    ("LinuxWrappedReady", "Итоги {0} готовы", "Insights {0} are ready"),
    ("LinuxWrappedReadyText", "Посмотрите, что вы слушали в этом году", "See what you listened to this year"),
    ("LinuxWrappedMinutes_one", "минута музыки за год", "minute of music this year"),
    ("LinuxWrappedMinutes_few", "минуты музыки за год", "minutes of music this year"),
    ("LinuxWrappedMinutes_many", "минут музыки за год", "minutes of music this year"),
    ("LinuxWrappedMinutes_other", "минуты музыки за год", "minutes of music this year"),
    ("LinuxWrappedTrack", "Трек года", "Track of the year"),
    ("LinuxWrappedMonth", "Любимый месяц", "Favorite month"),
    ("LinuxWrappedTime", "Любимое время суток", "Favorite time of day"),
    ("LinuxWrappedPeak", "Больше всего слушали около {0}", "Most listening around {0}"),
    ("LinuxWrappedNewTracks_one", "новый трек", "new track"),
    ("LinuxWrappedNewTracks_few", "новых трека", "new tracks"),
    ("LinuxWrappedNewTracks_many", "новых треков", "new tracks"),
    ("LinuxWrappedNewTracks_other", "нового трека", "new tracks"),
    ("LinuxWrappedDiscoveries", "Новое в этом году", "New this year"),
    ("LinuxWrappedEmpty", "За этот год пока нечего показать", "There is nothing to show for this year yet"),
    ("LinuxWrappedBack", "Назад", "Back"),
    ("LinuxWrappedNext", "Дальше", "Next"),
    ("LinuxWrappedPage", "Карточка {0} из {1}", "Card {0} of {1}"),
    ("LinuxShareWatermark", "Melogold · Итоги {0}", "Melogold · Insights {0}"),
    ("LinuxShareSaveAs", "Сохранить как…", "Save as…"),
    ("LinuxShareCopyImage", "Копировать картинку", "Copy image"),
    ("LinuxShareSaved", "Картинка сохранена", "Image saved"),
    ("LinuxShareCopied", "Картинка скопирована", "Image copied"),
    ("LinuxShareFailed", "Не удалось сделать картинку", "Couldn't make the picture"),
    ("LinuxShareFileType", "Картинка PNG", "PNG image"),
];

static LANG: OnceLock<Lang> = OnceLock::new();
static FORCED: OnceLock<bool> = OnceLock::new();

/// Язык выбран явно (настройка, `MELOGOLD_LANG`, снимки), а не взят у системы.
pub fn is_forced() -> bool {
    FORCED.get().copied().unwrap_or(false)
}

/// Язык процесса: выбранный в «Язык приложения» (или язык снимков), иначе язык системы.
///
/// Вызывается до GTK. Выбранный язык ставится и в `LANGUAGE`: свои подписи GTK и
/// libadwaita («Search shortcuts», «Close») берутся через gettext, и без этого окно
/// выходило бы на двух языках сразу.
pub fn init(forced: Option<Lang>) -> Lang {
    let _ = FORCED.set(forced.is_some());
    if let Some(lang) = forced {
        // Один поток, GTK ещё не запущен: переменная окружения меняется безопасно.
        std::env::set_var("LANGUAGE", if lang == Lang::Ru { "ru" } else { "en" });
    }
    *LANG.get_or_init(|| forced.unwrap_or_else(detect))
}

pub fn lang() -> Lang {
    *LANG.get_or_init(detect)
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
    lookup_in(lang(), key)
}

/// Строка на заданном языке — для тестов обоих языков в одном процессе.
pub fn lookup_in(lang: Lang, key: &str) -> Option<&'static str> {
    let pick = |ru: &'static str, en: &'static str| if lang == Lang::Ru { ru } else { en };
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
/// Сдвиг в секундах со знаком: «+0,5» по-русски, «+0.5» по-английски.
pub fn seconds_signed(ms: i64) -> String {
    let text = format!("{:+.1}", ms as f64 / 1000.0);
    if lang() == Lang::Ru {
        text.replace('.', ",")
    } else {
        text
    }
}

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
#[allow(dead_code)] // счётчики библиотеки — срез 4
pub fn plural(key: &'static str, count: i64) -> String {
    plural_in(lang(), key, count)
}

pub fn plural_in(lang: Lang, key: &'static str, count: i64) -> String {
    let form = plurals::form(count, lang == Lang::Ru);
    let full = format!("{key}_{}", form.suffix());
    let template = lookup_in(lang, &full).unwrap_or(key);
    format_args_into(template, &[&format_count_in(lang, count)])
}

/// Строка с подстановками на заданном языке.
pub fn trf_in(lang: Lang, key: &'static str, args: &[&dyn Display]) -> String {
    format_args_into(lookup_in(lang, key).unwrap_or(key), args)
}

/// Число с разделителем разрядов: «12 345» (неразрывный пробел) и «12,345».
#[allow(dead_code)] // счётчики библиотеки — срез 4
pub fn format_count(count: i64) -> String {
    format_count_in(lang(), count)
}

fn format_count_in(lang: Lang, count: i64) -> String {
    let digits = count.unsigned_abs().to_string();
    let separator = if lang == Lang::Ru { '\u{a0}' } else { ',' };
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
        // Язык процесса мог выбрать другой тест раньше: разделитель проверяется по языку явно.
        assert_eq!(format_count_in(Lang::Ru, 1234567), "1\u{a0}234\u{a0}567");
        assert_eq!(format_count_in(Lang::En, 1234567), "1,234,567");
        assert_eq!(format_count_in(Lang::Ru, 12), "12");
    }
}
