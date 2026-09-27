//! Исполнитель и название трека YouTube для поиска текста и «Других версий» (векторы
//! `spec/title-cleaner.vectors.json`, Windows `TitleCleaner.cs`). По порядку: эмодзи, `【…】`, всё
//! после « | », скобки из одного шума («(Official Video)», «[HD]»), номер трека, «Исполнитель -
//! Название», «feat.», кавычки и пробелы. Песни и клипы делят «Исполнитель - Название», только если
//! слева канал.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanTitle {
    pub artist: Option<String>,
    pub title: String,
}

macro_rules! re {
    ($name:ident, $pattern:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).expect(stringify!($name)));
    };
}

re!(LENTICULAR, "【[^】]*】");
re!(PIPE_TAIL, r"\s+\|.*$");
re!(BRACKETS, r"\s*[(\[]([^()\[\]]*)[)\]]");
re!(
    NOISE,
    r"(?i)^(?:official( (music|lyric))? (video|audio|visualizer|clip)|official|((music|lyric) )?video|audio|lyrics?|visualizer|hd|hq|4k|8k|1080p|720p|mv|m/v|официальное видео|официальный клип|клип|премьера( клипа)?(,.*)?|текст( песни)?)$"
);
re!(TRACK_NUMBER, r"^\d{1,3}\.\s+");
re!(SEPARATOR, " [-–—] ");
re!(FEAT_GROUP, r"(?i)\s*[(\[](feat\.?|ft\.?|featuring)\s[^)\]]*[)\]]");
re!(FEAT_TAIL, r"(?i)\s+(feat\.?|ft\.?|featuring)\s.*$");
re!(TOPIC, r"(?i)\s*[-–—]\s*(topic|тема)$");
re!(QUOTED, "^[«\"“„](.*)[»\"”“]$");
re!(SPACES, r"\s+");

const CHANNEL_TAILS: [&str; 2] = ["vevo", "official"];

pub fn clean(title: &str, channel: Option<&str>, video_type: Option<&str>) -> CleanTitle {
    let upload = matches!(video_type, None | Some("ugc") | Some("live"));
    let mut text = LENTICULAR.replace_all(&without_emoji(title), " ").into_owned();
    text = PIPE_TAIL.replace(&text, "").into_owned();
    text = BRACKETS
        .replace_all(&text, |c: &regex::Captures| if NOISE.is_match(c[1].trim()) { String::new() } else { c[0].to_owned() })
        .into_owned();
    text = collapse(&text);
    if upload {
        text = TRACK_NUMBER.replace(&text, "").into_owned();
    }
    let channel_artist = channel.map(|c| TOPIC.replace(c.trim(), "").into_owned()).filter(|c| !c.trim().is_empty());
    let mut artist = channel_artist.clone();
    if let Some(separator) = SEPARATOR.find(&text) {
        let left = text[..separator.start()].trim().to_owned();
        let is_channel = channel_artist.as_deref().is_some_and(|c| comparable(&left) == comparable(c));
        if is_channel || upload {
            artist = Some(without_feat(&left));
            text = text[separator.end()..].to_owned();
        }
    }
    text = collapse(&without_feat(&text));
    if let Some(quoted) = QUOTED.captures(&text) {
        text = collapse(&quoted[1]);
    }
    let artist = artist.map(|a| collapse(&a)).filter(|a| !a.is_empty());
    CleanTitle { artist, title: text }
}

fn without_feat(value: &str) -> String {
    FEAT_TAIL.replace(&FEAT_GROUP.replace_all(value, ""), "").trim().to_owned()
}

fn collapse(value: &str) -> String {
    SPACES.replace_all(value, " ").trim().to_owned()
}

/// Имя для сравнения канала и левой части: без feat, строчными, только буквы и цифры.
fn comparable(value: &str) -> String {
    let mut name: String = without_feat(value).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    for tail in CHANNEL_TAILS {
        if name.len() > tail.len() && name.ends_with(tail) {
            name.truncate(name.len() - tail.len());
        }
    }
    name
}

/// Эмодзи уходят: Extended_Pictographic, флаги, селектор варианта, соединители, keycap.
fn without_emoji(value: &str) -> String {
    value.chars().map(|c| if is_emoji_part(c as u32) { ' ' } else { c }).collect()
}

fn is_emoji_part(c: u32) -> bool {
    matches!(
        c,
        0xFE0F
            | 0x200D
            | 0x20E3
            | 0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2328
            | 0x23CF
            | 0x24C2
            | 0x25B6
            | 0x25C0
            | 0x2B50
            | 0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
    ) || (0x1F000..=0x1FAFF).contains(&c)
        || (0x1FC00..=0x1FFFD).contains(&c)
        || (0x2600..=0x27BF).contains(&c)
        || (0x2194..=0x2199).contains(&c)
        || (0x21A9..=0x21AA).contains(&c)
        || (0x231A..=0x231B).contains(&c)
        || (0x23E9..=0x23F3).contains(&c)
        || (0x23F8..=0x23FA).contains(&c)
        || (0x25AA..=0x25AB).contains(&c)
        || (0x25FB..=0x25FE).contains(&c)
        || (0x2934..=0x2935).contains(&c)
        || (0x2B05..=0x2B07).contains(&c)
        || (0x2B1B..=0x2B1C).contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_vectors() {
        let spec: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/title-cleaner.vectors.json")).unwrap(),
        )
        .unwrap();
        let cases = spec["cases"].as_array().unwrap();
        assert!(cases.len() >= 40);
        for case in cases {
            let input = &case["input"];
            let result = clean(input["title"].as_str().unwrap(), input["channel"].as_str(), input["videoType"].as_str());
            let expected = CleanTitle {
                artist: case["expected"]["artist"].as_str().map(str::to_owned),
                title: case["expected"]["title"].as_str().unwrap().to_owned(),
            };
            assert_eq!(result, expected, "{}", case["id"]);
        }
    }
}
