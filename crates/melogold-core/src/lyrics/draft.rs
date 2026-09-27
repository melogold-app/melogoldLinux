//! Черновик редактора текста (`spec/lyrics.md` «Редактор», Android `LyricsDraft`, Windows
//! `LyricsDraft.cs`): строки, строка (и в режиме слов — слово), которую отметит следующее нажатие, и
//! что отмечается — строки или слова. Каждая операция возвращает новый черновик: редактор хранит
//! прежние для «Отменить».

use std::sync::LazyLock;

use regex::Regex;

use super::{BackingVocals, LyricsAgent, LyricsTiming, SyncedLine, SyncedLyrics, SyncedWord, VocalSide};

/// Строка без своего конца и без следующей длится столько (как в LRC).
pub const DEFAULT_LINE_MS: i64 = 5_000;

static BACKING_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*\S)\s+(\([^()]*\))\s*$").expect("подпевка"));

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DraftLine {
    pub text: String,
    /// Начало строки в треке; `None` — ещё не отмечена.
    pub start_ms: Option<i64>,
    /// Свой конец строки — пауза до следующей; `None` — строку заканчивает следующая.
    pub end_ms: Option<i64>,
    /// Начала слов [`split_words`] в режиме слов; пусто — не отмечены.
    pub word_starts: Vec<Option<i64>>,
    pub side: VocalSide,
    /// Подпевка под строкой, в скобках: «(у-у)».
    pub backing: Option<String>,
    pub language: Option<String>,
}

impl DraftLine {
    pub fn new(text: impl Into<String>) -> DraftLine {
        DraftLine { text: text.into(), ..Default::default() }
    }

    pub fn words(&self) -> Vec<String> {
        split_words(&self.text)
    }

    /// У каждого слова есть начало (режим слов для строки закончен).
    pub fn words_timed(&self) -> bool {
        let count = self.words().len();
        count > 0 && self.word_starts.len() == count && self.word_starts.iter().all(Option::is_some)
    }

    /// Текст, как его показывает редактор: строка, затем подпевка.
    pub fn full_text(&self) -> String {
        match &self.backing {
            Some(backing) => format!("{} {backing}", self.text),
            None => self.text.clone(),
        }
    }
}

/// Слова строки, как их отмечает редактор: через пробел, знаки препинания — при слове.
pub fn split_words(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

fn in_parentheses(text: &str) -> String {
    if text.starts_with('(') && text.ends_with(')') {
        text.to_owned()
    } else {
        format!("({text})")
    }
}

fn agent_of(side: VocalSide) -> &'static str {
    if side == VocalSide::Start {
        "v1"
    } else {
        "v2"
    }
}

fn lines_of(text: &str) -> Vec<DraftLine> {
    text.split('\n')
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .map(|l| match BACKING_SUFFIX.captures(l) {
            Some(m) => DraftLine { text: m[1].to_owned(), backing: Some(m[2].to_owned()), ..Default::default() },
            None => DraftLine::new(l),
        })
        .collect()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LyricsDraft {
    pub lines: Vec<DraftLine>,
    pub cursor: usize,
    pub word_cursor: usize,
    pub timing: LyricsTiming,
    pub language: Option<String>,
}

impl LyricsDraft {
    /// Черновик из текста: строка на непустую строку, «(…)» в конце строки — подпевка.
    pub fn from_text(text: &str, language: Option<String>) -> LyricsDraft {
        LyricsDraft { lines: lines_of(text), language, ..Default::default() }
    }

    /// Черновик готового синхронного текста — чтобы его править.
    pub fn from_lyrics(lyrics: &SyncedLyrics) -> LyricsDraft {
        let mut lines: Vec<DraftLine> = lyrics
            .lines
            .iter()
            .map(|line| DraftLine {
                text: line.text.clone(),
                start_ms: Some(line.start_ms),
                end_ms: Some(line.end_ms),
                word_starts: if !line.words.is_empty() && line.words.len() == split_words(&line.text).len() {
                    line.words.iter().map(|w| Some(w.start_ms)).collect()
                } else {
                    Vec::new()
                },
                side: line.side,
                backing: line.background.as_ref().map(|b| b.text()).filter(|t| !t.is_empty()).map(|t| in_parentheses(&t)),
                language: line.language.clone(),
            })
            .collect();
        // Свой конец остаётся только там, где он оставляет паузу: в остальных местах строку заканчивает следующая.
        let starts: Vec<Option<i64>> = lines.iter().map(|l| l.start_ms).collect();
        for (index, line) in lines.iter_mut().enumerate() {
            if let (Some(end), Some(Some(next))) = (line.end_ms, starts.get(index + 1)) {
                if end >= *next {
                    line.end_ms = None;
                }
            }
        }
        LyricsDraft { cursor: lyrics.lines.len(), word_cursor: 0, timing: lyrics.timing, language: lyrics.language.clone(), lines }
    }

    /// Отмечена хотя бы одна строка — черновик даёт синхронный текст.
    pub fn has_timing(&self) -> bool {
        self.lines.iter().any(|l| l.start_ms.is_some())
    }

    /// Отмечены все строки.
    pub fn complete(&self) -> bool {
        !self.lines.is_empty() && self.lines.iter().all(|l| l.start_ms.is_some())
    }

    /// Черновик обычным текстом: строка на строку, подпевка в конце своей строки.
    pub fn to_text(&self) -> String {
        self.lines.iter().map(DraftLine::full_text).collect::<Vec<_>>().join("\n")
    }

    /// Отметить следующую строку (или слово) временем `position_ms` и перейти дальше. Начавшаяся
    /// строка заканчивает предыдущую, если у той нет своего конца ([`LyricsDraft::mark_end`]).
    pub fn mark(&self, position_ms: i64) -> LyricsDraft {
        let Some(line) = self.lines.get(self.cursor) else { return self.clone() };
        let position = position_ms.max(0);
        let words = line.words();
        if self.timing == LyricsTiming::Line || words.is_empty() {
            let mut lines = self.lines.clone();
            lines[self.cursor] = DraftLine { start_ms: Some(position), word_starts: Vec::new(), ..line.clone() };
            ending_previous_at(&mut lines, self.cursor, position);
            return LyricsDraft { lines, ..self.clone() }.moved_to(self.cursor + 1);
        }
        let mut starts: Vec<Option<i64>> = (0..words.len()).map(|i| line.word_starts.get(i).copied().flatten()).collect();
        let word = self.word_cursor.min(words.len() - 1);
        starts[word] = Some(position);
        let start = if word == 0 { Some(position) } else { line.start_ms.or(Some(position)) };
        let mut lines = self.lines.clone();
        lines[self.cursor] = DraftLine { start_ms: start, word_starts: starts, ..line.clone() };
        if word == 0 {
            ending_previous_at(&mut lines, self.cursor, position);
        }
        if word < words.len() - 1 {
            LyricsDraft { lines, word_cursor: word + 1, ..self.clone() }
        } else {
            LyricsDraft { lines, ..self.clone() }.moved_to(self.cursor + 1)
        }
    }

    /// Закончить последнюю отмеченную строку в `position_ms`: до следующей — пауза (проигрыш, если долгая).
    pub fn mark_end(&self, position_ms: i64) -> LyricsDraft {
        let Some(index) = self.cursor.checked_sub(1).map(|i| i.min(self.lines.len().saturating_sub(1))) else { return self.clone() };
        let Some(start) = self.lines.get(index).and_then(|l| l.start_ms) else { return self.clone() };
        self.update(index, |line| DraftLine { end_ms: Some(position_ms.max(start + 1)), ..line })
    }

    /// Сдвинуть начало строки `index` (и её слова) на `delta_ms`; время не меньше 0.
    pub fn nudge(&self, index: usize, delta_ms: i64) -> LyricsDraft {
        if self.lines.get(index).and_then(|l| l.start_ms).is_none() {
            return self.clone();
        }
        let moved = |v: i64| (v + delta_ms).max(0);
        self.update(index, |line| DraftLine {
            start_ms: line.start_ms.map(moved),
            end_ms: line.end_ms.map(moved),
            word_starts: line.word_starts.iter().map(|s| s.map(moved)).collect(),
            ..line
        })
    }

    /// Всё время на `delta_ms`: текст со сдвигом начала становится временем трека.
    pub fn shifted_by(&self, delta_ms: i64) -> LyricsDraft {
        if delta_ms == 0 {
            return self.clone();
        }
        let moved = |v: i64| (v + delta_ms).max(0);
        let lines = self
            .lines
            .iter()
            .map(|line| DraftLine {
                start_ms: line.start_ms.map(moved),
                end_ms: line.end_ms.map(moved),
                word_starts: line.word_starts.iter().map(|s| s.map(moved)).collect(),
                ..line.clone()
            })
            .collect();
        LyricsDraft { lines, ..self.clone() }
    }

    /// Забыть время строки `index`.
    pub fn clear_timing(&self, index: usize) -> LyricsDraft {
        self.update(index, |line| DraftLine { start_ms: None, end_ms: None, word_starts: Vec::new(), ..line })
    }

    /// Следующая отметка — строка `index` (с первого слова).
    pub fn moved_to(&self, index: usize) -> LyricsDraft {
        LyricsDraft { cursor: index.min(self.lines.len()), word_cursor: 0, ..self.clone() }
    }

    pub fn with_side(&self, index: usize, side: VocalSide) -> LyricsDraft {
        self.update(index, |line| DraftLine { side, ..line })
    }

    pub fn with_backing(&self, index: usize, backing: Option<&str>) -> LyricsDraft {
        let backing = backing.map(str::trim).filter(|b| !b.is_empty()).map(in_parentheses);
        self.update(index, |line| DraftLine { backing, ..line })
    }

    pub fn with_timing(&self, timing: LyricsTiming) -> LyricsDraft {
        LyricsDraft { timing, word_cursor: 0, ..self.clone() }
    }

    /// Черновик с новым текстом: у строк, текст которых не изменился (наибольшая общая
    /// подпоследовательность), остаются время, сторона и язык; следующая отметка — первая неотмеченная.
    pub fn with_text(&self, text: &str) -> LyricsDraft {
        let fresh = lines_of(text);
        let old: Vec<String> = self.lines.iter().map(DraftLine::full_text).collect();
        let new: Vec<String> = fresh.iter().map(DraftLine::full_text).collect();
        let matches = match_unchanged(&old, &new);
        let merged: Vec<DraftLine> = fresh
            .into_iter()
            .enumerate()
            .map(|(index, line)| match matches.get(&index) {
                Some(&old_index) => DraftLine { text: line.text, backing: line.backing, ..self.lines[old_index].clone() },
                None => line,
            })
            .collect();
        let first_untimed = merged.iter().position(|l| l.start_ms.is_none()).unwrap_or(merged.len());
        LyricsDraft { lines: merged, ..self.clone() }.moved_to(first_untimed)
    }

    /// Синхронный текст отмеченных строк по времени; `None` — не отмечено ничего. Строка без своего
    /// конца длится до начала следующей (последняя — [`DEFAULT_LINE_MS`]).
    pub fn to_synced(&self) -> Option<SyncedLyrics> {
        let mut timed: Vec<&DraftLine> = self.lines.iter().filter(|l| l.start_ms.is_some()).collect();
        if timed.is_empty() {
            return None;
        }
        timed.sort_by_key(|l| l.start_ms);
        let duet = timed.iter().any(|l| l.side == VocalSide::End);
        let word_timed = self.timing == LyricsTiming::Word && timed.iter().all(|l| l.words_timed());
        let lines = timed
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let start = line.start_ms.expect("отмечена");
                let next = timed.get(index + 1).and_then(|l| l.start_ms);
                let end = line.end_ms.or(next).unwrap_or(start + DEFAULT_LINE_MS).max(start + 1);
                SyncedLine {
                    start_ms: start,
                    end_ms: end,
                    text: line.text.clone(),
                    words: if word_timed { words_of(line, end) } else { Vec::new() },
                    agent: duet.then(|| agent_of(line.side).to_owned()),
                    side: line.side,
                    language: line.language.clone(),
                    background: line.backing.as_ref().map(|text| BackingVocals {
                        start_ms: start,
                        end_ms: end,
                        words: vec![SyncedWord::new(start, end, text.clone())],
                    }),
                    translation: None,
                    transliteration: None,
                }
            })
            .collect();
        let agents = if duet {
            vec![
                LyricsAgent { id: agent_of(VocalSide::Start).into(), side: VocalSide::Start, name: None },
                LyricsAgent { id: agent_of(VocalSide::End).into(), side: VocalSide::End, name: None },
            ]
        } else {
            Vec::new()
        };
        Some(SyncedLyrics {
            lines,
            timing: if word_timed { LyricsTiming::Word } else { LyricsTiming::Line },
            agents,
            language: self.language.clone(),
        })
    }

    fn update(&self, index: usize, transform: impl FnOnce(DraftLine) -> DraftLine) -> LyricsDraft {
        let Some(line) = self.lines.get(index) else { return self.clone() };
        let mut lines = self.lines.clone();
        lines[index] = transform(line.clone());
        LyricsDraft { lines, ..self.clone() }
    }
}

/// Слова строки: каждое до начала следующего, последнее — до `end`.
fn words_of(line: &DraftLine, end: i64) -> Vec<SyncedWord> {
    let words = line.words();
    words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let start = line.word_starts[index].expect("слово отмечено");
            let word_end = if index + 1 < words.len() { line.word_starts[index + 1].expect("слово отмечено") } else { end }.max(start + 1);
            let text = if index < words.len() - 1 { format!("{word} ") } else { word.clone() };
            SyncedWord::new(start, word_end, text)
        })
        .collect()
}

/// Последняя отмеченная строка до `index` заканчивается в `position`, если у неё нет своего конца раньше.
fn ending_previous_at(lines: &mut [DraftLine], index: usize, position: i64) {
    for i in (0..index).rev() {
        if lines[i].start_ms.is_none() {
            continue;
        }
        if lines[i].end_ms.is_some_and(|end| end <= position) {
            return;
        }
        lines[i].end_ms = None;
        return;
    }
}

/// Для строк нового текста — индекс той же строки в старом, если она в наибольшей общей подпоследовательности.
fn match_unchanged(old: &[String], fresh: &[String]) -> std::collections::HashMap<usize, usize> {
    let mut lengths = vec![vec![0usize; fresh.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..fresh.len()).rev() {
            lengths[i][j] = if old[i] == fresh[j] { lengths[i + 1][j + 1] + 1 } else { lengths[i + 1][j].max(lengths[i][j + 1]) };
        }
    }
    let mut matches = std::collections::HashMap::new();
    let (mut a, mut b) = (0, 0);
    while a < old.len() && b < fresh.len() {
        if old[a] == fresh[b] {
            matches.insert(b, a);
            a += 1;
            b += 1;
        } else if lengths[a + 1][b] >= lengths[a][b + 1] {
            a += 1;
        } else {
            b += 1;
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_lines_ends_and_backing() {
        let draft = LyricsDraft::from_text("Первая строка\n\nВторая (у-у)\nТретья", None);
        assert_eq!(draft.lines.len(), 3);
        assert_eq!(draft.lines[1].backing.as_deref(), Some("(у-у)"));
        let draft = draft.mark(1000).mark(3000).mark_end(4000).mark(10_000);
        let synced = draft.to_synced().unwrap();
        let spans: Vec<(i64, i64)> = synced.lines.iter().map(|l| (l.start_ms, l.end_ms)).collect();
        assert_eq!(spans, [(1000, 3000), (3000, 4000), (10_000, 15_000)]);
        assert_eq!(synced.lines[1].background.as_ref().unwrap().text(), "(у-у)");
        assert!(draft.complete());
    }

    #[test]
    fn words_mode_and_text_edits_keep_timing() {
        let draft = LyricsDraft::from_text("Hello world\nAgain", None).with_timing(LyricsTiming::Word);
        let draft = draft.mark(1000).mark(1500).mark(3000);
        let synced = draft.to_synced().unwrap();
        assert_eq!(synced.timing, LyricsTiming::Word);
        assert_eq!(synced.lines[0].words, [SyncedWord::new(1000, 1500, "Hello "), SyncedWord::new(1500, 3000, "world")]);
        // Правка текста: неизменённые строки сохраняют время, курсор — на первую неотмеченную.
        let edited = draft.with_text("Hello world\nNew line\nAgain");
        assert_eq!(edited.lines[0].start_ms, Some(1000));
        assert_eq!(edited.lines[1].start_ms, None);
        assert_eq!(edited.lines[2].start_ms, Some(3000));
        assert_eq!(edited.cursor, 1);
        // Сдвиг не уводит время ниже нуля.
        assert_eq!(edited.nudge(0, -2000).lines[0].start_ms, Some(0));
    }

    #[test]
    fn round_trip_through_synced_lyrics() {
        let lyrics = super::super::lrc::parse("[00:01.00]One\n[00:03.00]Two\n[00:04.00]\n[00:10.00]Three\n").unwrap();
        let draft = LyricsDraft::from_lyrics(&lyrics);
        assert_eq!(draft.lines[1].end_ms, Some(4000), "пауза перед третьей строкой сохраняется");
        assert_eq!(draft.lines[0].end_ms, None);
        assert_eq!(draft.to_synced().unwrap().lines, lyrics.lines);
    }
}
