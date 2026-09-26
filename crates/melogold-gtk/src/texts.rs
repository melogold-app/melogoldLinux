//! Тексты ошибок воспроизведения (REWRITE §3.10.9, задание 0001), как Windows `PlayerViewModel`.

use melogold_core::countries;
use melogold_playback::engine::PlayerError;
use melogold_playback::resolver::StreamErrorKind;

use crate::localization::{lang, lookup_in, plural_in, trf_in, Lang};

fn country(lang: Lang, code: &str) -> String {
    countries::name(code, lang == Lang::Ru)
}

fn tr(lang: Lang, key: &'static str) -> &'static str {
    lookup_in(lang, key).unwrap_or(key)
}

/// Текст по классу ошибки.
fn error_kind_text_in(lang: Lang, kind: StreamErrorKind) -> &'static str {
    tr(
        lang,
        match kind {
            StreamErrorKind::Network | StreamErrorKind::Timeout => "PlayErrorNetwork",
            StreamErrorKind::BotCheck => "PlayErrorBot",
            StreamErrorKind::Geo => "PlayErrorGeo",
            StreamErrorKind::Unavailable => "PlayErrorUnavailable",
            StreamErrorKind::Age => "PlayErrorAge",
            StreamErrorKind::Extractor => "PlayErrorExtractor",
        },
    )
}

/// Причина коротко — строка в панели плеера рядом с «Повторить»: «Недоступно: Россия».
pub fn error_title(error: &PlayerError) -> String {
    let lang = lang();
    match (&error.kind, &error.country) {
        (StreamErrorKind::Geo, Some(code)) => trf_in(lang, "PlayErrorGeoCountryShortFormat", &[&country(lang, code)]),
        _ => error_kind_text_in(lang, error.kind).to_owned(),
    }
}

/// Причина в плашке «Пропущен „…“» — абзац за 4 секунды не прочитать: «Недоступно в стране «Россия»».
fn error_notice_in(lang: Lang, error: &PlayerError) -> String {
    match (&error.kind, &error.country) {
        (StreamErrorKind::Geo, Some(code)) => trf_in(lang, "PlayErrorGeoCountryNoticeFormat", &[&country(lang, code)]),
        _ => error_kind_text_in(lang, error.kind).to_owned(),
    }
}

/// Причина целиком — карточка ошибки: со страной, где YouTube видит устройство, и числом стран,
/// где трек открыт.
pub fn error_text(error: &PlayerError) -> String {
    error_text_in(lang(), error)
}

fn error_text_in(lang: Lang, error: &PlayerError) -> String {
    let (StreamErrorKind::Geo, Some(code)) = (&error.kind, &error.country) else {
        return error_kind_text_in(lang, error.kind).to_owned();
    };
    let name = country(lang, code);
    match error.open_countries {
        Some(open) => trf_in(lang, "PlayErrorGeoCountryOpenFormat", &[&name, &plural_in(lang, "GeoOtherCountries", open as i64)]),
        None => trf_in(lang, "PlayErrorGeoCountryFormat", &[&name]),
    }
}

/// «Пропущен „…“: причина».
pub fn skipped(error: &PlayerError) -> String {
    let lang = lang();
    trf_in(lang, "SkippedFormat", &[&error.track.title, &error_notice_in(lang, error)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use melogold_core::music::Track;

    fn geo(country: Option<&str>, open: Option<usize>) -> PlayerError {
        PlayerError {
            kind: StreamErrorKind::Geo,
            message: String::new(),
            track: Track { title: "Photosynthesis".into(), ..Default::default() },
            country: country.map(str::to_owned),
            open_countries: open,
        }
    }

    /// Тексты задания 0001 — слово в слово, на обоих языках.
    #[test]
    fn geo_texts_in_both_languages() {
        let ru = Lang::Ru;
        assert_eq!(
            error_text_in(ru, &geo(Some("RU"), Some(122))),
            "Недоступно в стране «Россия»: YouTube считает, что вы там, а правообладатель открыл трек в 122 других странах. \
             С VPN выберите сервер другой страны: некоторые серверы YouTube тоже относит к стране «Россия»."
        );
        assert!(error_text_in(ru, &geo(Some("RU"), Some(121))).contains("в 121 другой стране."));
        assert_eq!(
            error_text_in(ru, &geo(Some("RU"), None)),
            "Недоступно в стране «Россия»: YouTube считает, что вы там, а правообладатель закрыл трек для этой страны. \
             С VPN выберите сервер другой страны: некоторые серверы YouTube тоже относит к стране «Россия»."
        );
        assert_eq!(error_text_in(ru, &geo(None, None)), "Недоступно в вашей стране");
        assert_eq!(error_notice_in(ru, &geo(Some("RU"), Some(122))), "Недоступно в стране «Россия»");

        let en = Lang::En;
        assert_eq!(
            error_text_in(en, &geo(Some("RU"), Some(122))),
            "Unavailable in Russia: YouTube places you there, and the rights holder opened this track in 122 other countries. \
             With a VPN, pick a server in another country: YouTube counts some VPN servers as Russia too."
        );
        assert!(error_text_in(en, &geo(Some("RU"), Some(1))).contains("in 1 other country."));
        assert_eq!(
            error_text_in(en, &geo(Some("RU"), None)),
            "Unavailable in Russia: YouTube places you there, and the rights holder closed this track for it. \
             With a VPN, pick a server in another country: YouTube counts some VPN servers as Russia too."
        );
        assert_eq!(error_text_in(en, &geo(None, None)), "Unavailable in your country");
    }
}
