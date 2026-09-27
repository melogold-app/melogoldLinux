//! LRC и расширенный LRC (A2): `[мм:сс.xx]строка`, несколько меток у строки, `[offset:±мс]`, метки
//! слов `<мм:сс.xx>` и дуэты «walaoke» `M:`, `F:`, `D:` (`spec/lyrics.md`, Android `LrcFormat.kt`).

use std::fmt::Write;
use std::sync::LazyLock;

use regex::Regex;

use super::{assign_sides, LyricsTiming, SyncedLine, SyncedLyrics, SyncedWord, VocalSide};

static LINE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?]").expect("метка строки"));
static META_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\[([a-zA-Z#]+):(.*)]\s*$").expect("метка сведений"));
static WORD_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?>").expect("метка слова"));
static WALAOKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*([MFD]):\s?").expect("дуэт"));

const LAST_LINE_MS: i64 = 5_000;

fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

/// Похоже на LRC: хотя бы одна строка начинается с метки времени.
pub fn matches(text: &str) -> bool {
    lines(text).any(|l| LINE_TAG.is_match(l.trim()))
}

struct RawLine {
    start_ms: i64,
    text: String,
    words: Option<Vec<SyncedWord>>,
    agent: Option<String>,
}

fn millis(captures: &regex::Captures) -> i64 {
    let minutes: i64 = captures[1].parse().unwrap_or(0);
    let seconds: i64 = captures[2].parse().unwrap_or(0);
    let fraction = captures.get(3).map(|m| m.as_str()).unwrap_or("");
    let millis: i64 = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<i64>().unwrap_or(0) * 100,
        2 => fraction.parse::<i64>().unwrap_or(0) * 10,
        _ => fraction[..3].parse().unwrap_or(0),
    };
    (minutes * 60 + seconds) * 1000 + millis
}

/// Разбор LRC; `None`, если ни одна строка не размечена временем.
pub fn parse(text: &str) -> Option<SyncedLyrics> {
    let mut offset_ms = 0i64;
    let mut agent: Option<String> = None;
    let mut raw: Vec<RawLine> = Vec::new();
    for source in lines(text) {
        let mut line = source.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(meta) = META_TAG.captures(line) {
            if meta[1].eq_ignore_ascii_case("offset") {
                offset_ms = meta[2].trim().trim_start_matches('+').parse().unwrap_or(0);
            }
            continue;
        }
        // Все метки перед текстом.
        let mut starts = Vec::new();
        while let Some(tag) = LINE_TAG.captures(line) {
            starts.push(millis(&tag));
            line = &line[tag.get(0).expect("метка").end()..];
        }
        if starts.is_empty() {
            continue;
        }
        if let Some(duet) = WALAOKE.captures(line) {
            agent = Some(duet[1].to_owned());
            line = &line[duet.get(0).expect("метка").end()..];
        }
        let words = parse_words(line);
        let line_text = match &words {
            Some(words) if !words.is_empty() => words.iter().map(|(_, t)| t.as_str()).collect::<String>().trim().to_owned(),
            _ => line.trim().to_owned(),
        };
        for start in starts {
            let shifted = words.as_ref().map(|w| shift_words(w, start, start - offset_ms));
            raw.push(RawLine { start_ms: (start - offset_ms).max(0), text: line_text.clone(), words: shifted, agent: agent.clone() });
        }
    }
    raw.sort_by_key(|r| r.start_ms);
    if raw.iter().all(|r| r.text.trim().is_empty()) {
        return None;
    }
    let agents = assign_sides(raw.iter().filter_map(|r| r.agent.as_deref()));
    let word_timed = raw.iter().any(|r| r.words.as_ref().is_some_and(|w| !w.is_empty()));
    let mut result = Vec::new();
    for (index, line) in raw.iter().enumerate() {
        // Пустая строка с меткой только отмечает конец предыдущей.
        if line.text.trim().is_empty() {
            continue;
        }
        let next = raw.get(index + 1).map(|r| r.start_ms);
        // Последняя метка без текста («…слово<00:13.20>») — конец строки.
        let explicit_end = line.words.as_ref().and_then(|all| all.last()).filter(|w| w.text.is_empty()).map(|w| w.start_ms);
        let end = match (explicit_end, next) {
            (Some(e), _) if e > line.start_ms => e,
            (_, Some(n)) if n > line.start_ms => n,
            _ => line.start_ms + LAST_LINE_MS,
        };
        let words: Vec<SyncedWord> = line
            .words
            .iter()
            .flatten()
            .filter(|w| !w.text.is_empty())
            .map(|w| SyncedWord { end_ms: if w.end_ms > w.start_ms { w.end_ms } else { end }, ..w.clone() })
            .collect();
        let side = line.agent.as_ref().and_then(|id| agents.iter().find(|a| &a.id == id)).map(|a| a.side).unwrap_or(VocalSide::Start);
        result.push(SyncedLine {
            start_ms: line.start_ms,
            end_ms: end,
            text: line.text.clone(),
            words,
            agent: line.agent.clone(),
            side,
            ..Default::default()
        });
    }
    Some(SyncedLyrics { lines: result, timing: if word_timed { LyricsTiming::Word } else { LyricsTiming::Line }, agents, language: None })
}

fn parse_words(line: &str) -> Option<Vec<(i64, String)>> {
    let tags: Vec<regex::Captures> = WORD_TAG.captures_iter(line).collect();
    if tags.is_empty() {
        return None;
    }
    Some(
        tags.iter()
            .enumerate()
            .map(|(i, tag)| {
                let text_start = tag.get(0).expect("метка").end();
                let text_end = tags.get(i + 1).map(|n| n.get(0).expect("метка").start()).unwrap_or(line.len());
                (millis(tag), line[text_start..text_end].to_owned())
            })
            .collect(),
    )
}

/// Слово кончается там, где начинается следующее; пустая метка в конце заканчивает последнее.
fn shift_words(words: &[(i64, String)], tag_base: i64, line_start: i64) -> Vec<SyncedWord> {
    let delta = line_start - tag_base;
    words
        .iter()
        .enumerate()
        .map(|(i, (start, text))| {
            let end = words.get(i + 1).map(|w| w.0).unwrap_or(*start);
            SyncedWord::new((start + delta).max(0), (end + delta).max(0), text.clone())
        })
        .collect()
}

/// `мм:сс.xx`, или `мм:сс.xxx`, если сотые потеряли бы время.
pub fn timestamp(ms: i64) -> String {
    let total = ms.max(0);
    let (minutes, seconds, millis) = (total / 60_000, total / 1000 % 60, total % 1000);
    if millis % 10 == 0 {
        format!("{minutes:02}:{seconds:02}.{:02}", millis / 10)
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

/// LRC: расширенный (с метками слов), если текст размечен по словам; дуэт из 2–3 исполнителей — walaoke.
pub fn write(lyrics: &SyncedLyrics, enhanced: bool) -> String {
    let mut out = String::new();
    let walaoke = (2..=3).contains(&lyrics.agents.len());
    let prefix = |id: &str| lyrics.agents.iter().take(3).position(|a| a.id == id).map(|i| ['M', 'F', 'D'][i]);
    let mut last_agent: Option<&str> = None;
    for (index, line) in lyrics.lines.iter().enumerate() {
        let _ = write!(out, "[{}]", timestamp(line.start_ms));
        if let Some(agent) = line.agent.as_deref().filter(|a| walaoke && Some(*a) != last_agent) {
            if let Some(p) = prefix(agent) {
                let _ = write!(out, "{p}: ");
                last_agent = Some(agent);
            }
        }
        if enhanced && !line.words.is_empty() {
            for word in &line.words {
                let _ = write!(out, "<{}>{}", timestamp(word.start_ms), word.text);
            }
            let _ = write!(out, "<{}>", timestamp(line.words.last().expect("слова").end_ms));
        } else {
            out.push_str(&line.text);
        }
        out.push('\n');
        // Пауза перед следующей строкой — закрыть эту пустой строкой.
        if lyrics.lines.get(index + 1).is_none_or(|next| next.start_ms > line.end_ms) {
            let _ = writeln!(out, "[{}]", timestamp(line.end_ms));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_lrc() {
        let text = "[00:01.00]M: <00:01.00>Hello <00:01.50>world<00:02.20>\n[00:03.00]F: Again\n";
        let lyrics = parse(text).unwrap();
        let written = write(&lyrics, true);
        assert_eq!(parse(&written).unwrap(), lyrics);
        assert_eq!(timestamp(62_345), "01:02.345");
        assert_eq!(timestamp(62_340), "01:02.34");
    }
}
