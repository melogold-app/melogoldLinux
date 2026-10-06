//! Описание альбома или исполнителя из YouTube Music и его источник (задание 0023, Windows
//! `DescriptionText.cs`).
//!
//! Разбора Википедии у клиента нет: YouTube сам берёт текст из статьи и дописывает в конец строку
//! «From Wikipedia (адрес) under Creative Commons Attribution CC-BY-SA 3.0 (адрес)» (по-русски — «Из
//! Википедии (…) по лицензии …»). Здесь эта строка отделяется от текста, чтобы показать её ссылками.

use std::sync::OnceLock;

use regex::Regex;
use url::Url;

/// Откуда взято описание: статья Википедии и лицензия.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptionSource {
    /// Адрес статьи на `*.wikipedia.org` (https).
    pub article_url: String,
    /// Название лицензии: «Creative Commons Attribution CC-BY-SA 3.0»; `None` — не указано.
    pub license: Option<String>,
    /// Адрес текста лицензии; `None` — YouTube его обрезал, а по названию адрес не восстановить.
    pub license_url: Option<String>,
}

fn url_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"https?://[^\s)]+").expect("правильное выражение"))
}

fn license_lead() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)^(?:under|по\s+лицензии|по|—|-)\s+").expect("правильное выражение"))
}

/// Текст без строки об источнике и сам источник (`None` — строки нет или адрес не Википедии).
pub fn split(description: Option<&str>) -> (String, Option<DescriptionSource>) {
    let text = description.unwrap_or_default().trim();
    if text.is_empty() {
        return (String::new(), None);
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let Some(index) = lines.iter().rposition(|line| line.to_lowercase().contains("wikipedia.org")) else {
        return (text.to_owned(), None);
    };
    // Строка об источнике — последняя: после неё текста нет.
    if lines[index + 1..].iter().any(|line| !line.trim().is_empty()) {
        return (text.to_owned(), None);
    }
    match parse(lines[index]) {
        Some(source) => (lines[..index].join("\n").trim().to_owned(), Some(source)),
        None => (text.to_owned(), None),
    }
}

fn parse(line: &str) -> Option<DescriptionSource> {
    let article = url_pattern().find_iter(line).find(|m| is_wikipedia(m.as_str()))?;
    // После «(адрес статьи)» — «under Creative Commons … 3.0 (адрес лицензии)».
    let rest = line[article.end()..].trim_start_matches([')', ' ', '\t']).trim();
    let found = url_pattern().find(rest);
    let name = match found {
        Some(m) => &rest[..m.start()],
        None => rest,
    };
    let name = name.trim().trim_end_matches(['(', ' ', '.']);
    let name = license_lead().replace(name, "").trim().to_owned();
    let license = (!name.is_empty()).then_some(name);
    let mut license_url = found.map(|m| m.as_str().to_owned());
    // YouTube обрезает адрес («…/licenses/...»): такой не открыть — восстанавливается по названию.
    if license_url.as_deref().is_some_and(|u| u.ends_with("...") || u.ends_with('…')) {
        license_url = license.as_deref().and_then(known_license);
    }
    Some(DescriptionSource { article_url: article.as_str().to_owned(), license, license_url })
}

fn known_license(name: &str) -> Option<String> {
    let upper = name.to_uppercase();
    let has = |version: &str| upper.contains(&format!("CC-BY-SA {version}")) || upper.contains(&format!("CC BY-SA {version}"));
    if has("3.0") {
        Some("https://creativecommons.org/licenses/by-sa/3.0/".into())
    } else if has("4.0") {
        Some("https://creativecommons.org/licenses/by-sa/4.0/".into())
    } else {
        None
    }
}

fn https_host_within(url: &str, domain: &str) -> bool {
    let Ok(url) = Url::parse(url) else { return false };
    url.scheme() == "https"
        && url.host_str().is_some_and(|host| {
            let host = host.to_ascii_lowercase();
            host == domain || host.ends_with(&format!(".{domain}"))
        })
}

/// Википедия по https: только такой адрес открывается из окна (описание приходит из сети).
pub fn is_wikipedia(url: &str) -> bool {
    https_host_within(url, "wikipedia.org")
}

/// Лицензия открывается только с `creativecommons.org` по https.
pub fn is_license(url: &str) -> bool {
    https_host_within(url, "creativecommons.org")
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "Abbey Road is the eleventh studio album by the Beatles.\nIt is the last album the group recorded.";

    #[test]
    fn english_footer_from_youtube_music() {
        let text = format!(
            "{BODY}\n\nFrom Wikipedia (https://en.wikipedia.org/wiki/Abbey_Road) under Creative Commons Attribution CC-BY-SA 3.0 (https://creativecommons.org/licenses/...)"
        );
        let (body, source) = split(Some(&text));
        assert_eq!(body, BODY);
        let source = source.expect("источник");
        assert_eq!(source.article_url, "https://en.wikipedia.org/wiki/Abbey_Road");
        assert_eq!(source.license.as_deref(), Some("Creative Commons Attribution CC-BY-SA 3.0"));
        // YouTube обрезал адрес лицензии — он восстановлен по названию.
        assert_eq!(source.license_url.as_deref(), Some("https://creativecommons.org/licenses/by-sa/3.0/"));
    }

    #[test]
    fn russian_footer_with_full_license_link() {
        let text = "Альбом группы «Кино».\n\nИз Википедии (https://ru.wikipedia.org/wiki/%D0%93%D1%80%D1%83%D0%BF%D0%BF%D0%B0_%D0%BA%D1%80%D0%BE%D0%B2%D0%B8) по лицензии Creative Commons Attribution CC-BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/)";
        let (body, source) = split(Some(text));
        assert_eq!(body, "Альбом группы «Кино».");
        let source = source.expect("источник");
        assert!(source.article_url.starts_with("https://ru.wikipedia.org/wiki/"));
        assert_eq!(source.license.as_deref(), Some("Creative Commons Attribution CC-BY-SA 3.0"));
        assert_eq!(source.license_url.as_deref(), Some("https://creativecommons.org/licenses/by-sa/3.0/"));
    }

    #[test]
    fn footer_without_license_and_truncated_unknown_license() {
        let (body, source) = split(Some(&format!("{BODY}\n\nFrom Wikipedia (https://en.wikipedia.org/wiki/Abbey_Road)")));
        assert_eq!(body, BODY);
        assert_eq!(
            source,
            Some(DescriptionSource { article_url: "https://en.wikipedia.org/wiki/Abbey_Road".into(), license: None, license_url: None })
        );
        let other = split(Some(&format!(
            "{BODY}\n\nFrom Wikipedia (https://en.wikipedia.org/wiki/X) under Some License (https://example.org/l...)"
        )))
        .1
        .expect("источник");
        assert_eq!(other.license.as_deref(), Some("Some License"));
        assert_eq!(other.license_url, None);
    }

    #[test]
    fn an_empty_description_is_empty() {
        assert_eq!(split(Some("")), (String::new(), None));
        assert_eq!(split(Some("   ")), (String::new(), None));
        assert_eq!(split(None), (String::new(), None));
    }

    #[test]
    fn text_without_a_footer_stays_whole() {
        assert_eq!(split(Some(BODY)), (BODY.to_owned(), None));
        // Вики упомянута в самом тексте, и после неё есть ещё текст — это не сноска.
        let in_text = "See https://en.wikipedia.org/wiki/Abbey_Road for more.\nAnd another line.";
        assert_eq!(split(Some(in_text)), (in_text.to_owned(), None));
    }

    #[test]
    fn only_https_wikipedia_and_creative_commons_open() {
        assert!(is_wikipedia("https://en.wikipedia.org/wiki/Abbey_Road"));
        assert!(!is_wikipedia("http://en.wikipedia.org/wiki/Abbey_Road"));
        assert!(!is_wikipedia("https://evilwikipedia.org/wiki/x"));
        assert!(!is_wikipedia("https://wikipedia.org.evil.example/wiki/x"));
        assert!(is_license("https://creativecommons.org/licenses/by-sa/3.0/"));
        assert!(!is_license("https://example.org/licenses/by-sa/3.0/"));
        // Адрес не Википедии в сноске — не источник.
        assert_eq!(split(Some(&format!("{BODY}\n\nFrom wikipedia.org (https://evil.example/x)"))).1, None);
    }

    #[test]
    fn a_cc_by_sa_4_license_is_restored_too() {
        let source =
            split(Some("Text.\nFrom Wikipedia (https://de.wikipedia.org/wiki/X) under CC BY-SA 4.0 (https://creativecommons.org/…)"))
                .1
                .expect("источник");
        assert_eq!(source.license_url.as_deref(), Some("https://creativecommons.org/licenses/by-sa/4.0/"));
    }
}
