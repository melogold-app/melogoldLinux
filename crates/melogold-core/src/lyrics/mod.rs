//! Тексты песен в модели Melogold (`spec/lyrics.md`, Android `SyncedLyrics.kt`, Windows
//! `Melogold.Core/Lyrics`): строки, время слов, стороны дуэта, подпевка, языки, переводы. Читается из
//! LRC, расширенного LRC и TTML, пишется в TTML и LRC; редактор — [`draft::LyricsDraft`].

pub mod draft;
pub mod lrc;
pub mod pins;
pub mod rows;
pub mod sync_rules;
pub mod ttml;

/// Точность времени текста: строки целиком или каждое слово (слог).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LyricsTiming {
    #[default]
    Line,
    Word,
}

/// Сторона голоса в дуэте: первый исполнитель у начального края, второй — у конечного (в RTL зеркально).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VocalSide {
    #[default]
    Start,
    End,
}

/// Исполнитель дуэта (`ttm:agent`); сторона — по порядку объявления или появления.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LyricsAgent {
    pub id: String,
    pub side: VocalSide,
    pub name: Option<String>,
}

/// Слово или слог со временем. Пробел после слова входит в текст: склейка слов строки даёт строку.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncedWord {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

impl SyncedWord {
    pub fn new(start_ms: i64, end_ms: i64, text: impl Into<String>) -> SyncedWord {
        SyncedWord { start_ms, end_ms, text: text.into() }
    }
}

/// Подпевка строки (`ttm:role="x-bg"`): мельче, под основной строкой.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackingVocals {
    pub start_ms: i64,
    pub end_ms: i64,
    pub words: Vec<SyncedWord>,
}

impl BackingVocals {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_owned()
    }
}

/// Строка синхронного текста.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncedLine {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    /// Слова со временем; пусто, если время есть только у строки.
    pub words: Vec<SyncedWord>,
    /// Исполнитель ([`SyncedLyrics::agents`]); `None`, если текст не говорит.
    pub agent: Option<String>,
    pub side: VocalSide,
    /// BCP 47, если отличается от языка всего текста или уточняет его.
    pub language: Option<String>,
    pub background: Option<BackingVocals>,
    pub translation: Option<String>,
    pub transliteration: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncedLyrics {
    pub lines: Vec<SyncedLine>,
    pub timing: LyricsTiming,
    pub agents: Vec<LyricsAgent>,
    pub language: Option<String>,
}

impl SyncedLyrics {
    pub fn is_duet(&self) -> bool {
        self.lines.iter().any(|l| l.side == VocalSide::End)
    }

    /// Текст без времени: строка на строку (для «обычного» вида и поиска).
    pub fn plain_text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }
}

/// Стороны по порядку появления: первый — у начала, второй — у конца, дальше по очереди.
pub(crate) fn assign_sides<'a>(ids: impl IntoIterator<Item = &'a str>) -> Vec<LyricsAgent> {
    let mut seen: Vec<&str> = Vec::new();
    for id in ids {
        if !seen.contains(&id) {
            seen.push(id);
        }
    }
    seen.into_iter()
        .enumerate()
        .map(|(index, id)| LyricsAgent {
            id: id.to_owned(),
            side: if index % 2 == 0 { VocalSide::Start } else { VocalSide::End },
            name: None,
        })
        .collect()
}

/// Формат файла текста по содержимому.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Ttml,
    Lrc,
    Plain,
}

pub fn detect(text: &str) -> Format {
    if ttml::matches(text) {
        Format::Ttml
    } else if lrc::matches(text) {
        Format::Lrc
    } else {
        Format::Plain
    }
}

/// Синхронный текст из TTML или LRC; `None` для простого текста или битого файла.
pub fn parse_synced(text: &str) -> Option<SyncedLyrics> {
    match detect(text) {
        Format::Ttml => ttml::parse(text),
        Format::Lrc => lrc::parse(text),
        Format::Plain => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn side(value: &Value) -> VocalSide {
        if value == "End" {
            VocalSide::End
        } else {
            VocalSide::Start
        }
    }

    fn words(value: &Value) -> Vec<SyncedWord> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|w| SyncedWord::new(w[0].as_i64().unwrap(), w[1].as_i64().unwrap(), w[2].as_str().unwrap()))
            .collect()
    }

    fn text(value: &Value) -> Option<String> {
        value.as_str().map(str::to_owned)
    }

    #[test]
    fn shared_vectors() {
        let spec: Value =
            serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/lyrics.vectors.json")).unwrap())
                .unwrap();
        for case in spec["cases"].as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            let parsed = parse_synced(input);
            let expected = &case["expected"];
            if expected.is_null() {
                assert_eq!(parsed, None, "{}", case["id"]);
                continue;
            }
            let expected = SyncedLyrics {
                timing: if expected["timing"] == "Word" { LyricsTiming::Word } else { LyricsTiming::Line },
                language: text(&expected["language"]),
                agents: expected["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| LyricsAgent { id: a["id"].as_str().unwrap().into(), side: side(&a["side"]), name: text(&a["name"]) })
                    .collect(),
                lines: expected["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| SyncedLine {
                        start_ms: l["startMs"].as_i64().unwrap(),
                        end_ms: l["endMs"].as_i64().unwrap(),
                        text: l["text"].as_str().unwrap().into(),
                        words: words(&l["words"]),
                        side: side(&l["side"]),
                        agent: text(&l["agent"]),
                        language: text(&l["language"]),
                        background: (!l["background"].is_null()).then(|| BackingVocals {
                            start_ms: l["background"]["startMs"].as_i64().unwrap(),
                            end_ms: l["background"]["endMs"].as_i64().unwrap(),
                            words: words(&l["background"]["words"]),
                        }),
                        translation: text(&l["translation"]),
                        transliteration: text(&l["transliteration"]),
                    })
                    .collect(),
            };
            assert_eq!(parsed, Some(expected), "{}", case["id"]);
        }
    }
}
