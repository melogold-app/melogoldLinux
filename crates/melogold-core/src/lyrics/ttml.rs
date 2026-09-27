//! TTML, как пишут Apple Music и база AMLL TTML: строки `p` со словами `span`, исполнители
//! `ttm:agent`, подпевка `x-bg`, перевод и транскрипция, `xml:lang`. В нём Melogold хранит тексты и
//! делится ими. DTD и внешние сущности запрещены: файлы текстов приходят откуда угодно.

use std::fmt::Write;

use roxmltree::{Document, Node, ParsingOptions};

use super::{assign_sides, BackingVocals, LyricsAgent, LyricsTiming, SyncedLine, SyncedLyrics, SyncedWord, VocalSide};

const NS_TTML: &str = "http://www.w3.org/ns/ttml";
const NS_TTM: &str = "http://www.w3.org/ns/ttml#metadata";
const NS_ITUNES: &str = "http://music.apple.com/lyric-ttml-internal";
const NS_XML: &str = "http://www.w3.org/XML/1998/namespace";

pub fn matches(text: &str) -> bool {
    let head = text.trim_start();
    let head = &head[..head.char_indices().nth(512).map(|(i, _)| i).unwrap_or(head.len())];
    (head.starts_with("<?xml") || head.starts_with("<tt")) && head.contains("<tt")
}

fn attr<'a>(node: Node<'a, '_>, ns: Option<&str>, name: &str) -> Option<&'a str> {
    let value = match ns {
        Some(ns) => node.attribute((ns, name)),
        None => node.attribute(name),
    };
    value.filter(|v| !v.is_empty())
}

fn inner_text(node: Node) -> String {
    node.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect()
}

/// Разбор TTML; `None`, если это не TTML или в нём нет строк со временем.
pub fn parse(text: &str) -> Option<SyncedLyrics> {
    let document = Document::parse_with_options(text, ParsingOptions { allow_dtd: false, ..Default::default() }).ok()?;
    let root = document.root_element();
    if root.tag_name().name() != "tt" {
        return None;
    }
    let declared: Vec<(String, Option<String>)> = root
        .descendants()
        .filter(|e| e.is_element() && e.tag_name().name() == "agent")
        .filter_map(|a| {
            let id = attr(a, Some(NS_XML), "id")?.to_owned();
            let name = a
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "name")
                .map(|n| inner_text(n).trim().to_owned())
                .filter(|n| !n.is_empty());
            Some((id, name))
        })
        .collect();
    let mut parsed: Vec<SyncedLine> =
        root.descendants().filter(|e| e.is_element() && e.tag_name().name() == "p").filter_map(parse_line).collect();
    if parsed.is_empty() {
        return None;
    }
    parsed.sort_by_key(|l| l.start_ms);
    let order: Vec<&str> = declared.iter().map(|(id, _)| id.as_str()).chain(parsed.iter().filter_map(|l| l.agent.as_deref())).collect();
    let agents: Vec<LyricsAgent> = assign_sides(order)
        .into_iter()
        .map(|a| LyricsAgent { name: declared.iter().find(|(id, _)| *id == a.id).and_then(|(_, n)| n.clone()), ..a })
        .collect();
    let timing = match attr(root, Some(NS_ITUNES), "timing") {
        Some("Line") => LyricsTiming::Line,
        Some("Word") => LyricsTiming::Word,
        _ if parsed.iter().any(|l| !l.words.is_empty()) => LyricsTiming::Word,
        _ => LyricsTiming::Line,
    };
    let lines = parsed
        .into_iter()
        .map(|l| {
            let side = l.agent.as_ref().and_then(|id| agents.iter().find(|a| &a.id == id)).map(|a| a.side).unwrap_or(VocalSide::Start);
            SyncedLine { side, ..l }
        })
        .collect();
    Some(SyncedLyrics { lines, timing, agents, language: attr(root, Some(NS_XML), "lang").map(str::to_owned) })
}

#[derive(Default)]
struct Collector {
    background: Vec<SyncedWord>,
    background_text: String,
    translation: Option<String>,
    transliteration: Option<String>,
}

impl Collector {
    fn collect(&mut self, parent: Node, target: &mut Vec<SyncedWord>, mut plain: Option<&mut String>) {
        for node in parent.children() {
            if node.is_text() {
                let value = node.text().unwrap_or_default();
                if !target.is_empty() && !value.is_empty() && value.trim().is_empty() {
                    let last = target.last_mut().expect("слово");
                    if !last.text.ends_with(' ') {
                        last.text.push(' ');
                    }
                } else if let Some(plain) = plain.as_deref_mut() {
                    plain.push_str(value);
                }
                continue;
            }
            if !node.is_element() || node.tag_name().name() != "span" {
                continue;
            }
            match attr(node, Some(NS_TTM), "role") {
                Some("x-bg") => {
                    let mut words = std::mem::take(&mut self.background);
                    self.collect(node, &mut words, None);
                    self.background = words;
                    if self.background.is_empty() {
                        self.background_text = inner_text(node).trim().to_owned();
                    }
                }
                Some("x-translation") => self.translation = Some(inner_text(node).trim().to_owned()).filter(|t| !t.is_empty()),
                Some("x-roman") => self.transliteration = Some(inner_text(node).trim().to_owned()).filter(|t| !t.is_empty()),
                _ => {
                    let begin = attr(node, None, "begin").and_then(parse_time);
                    let end = attr(node, None, "end").and_then(parse_time);
                    match (begin, end) {
                        (Some(begin), Some(end)) => target.push(SyncedWord::new(begin, end, inner_text(node))),
                        _ => self.collect(node, target, plain.as_deref_mut()),
                    }
                }
            }
        }
    }
}

/// У последнего слова нет пробела в конце.
fn trim_last(mut words: Vec<SyncedWord>) -> Vec<SyncedWord> {
    if let Some(last) = words.last_mut() {
        last.text = last.text.trim_end().to_owned();
    }
    words
}

fn parse_line(p: Node) -> Option<SyncedLine> {
    let mut words = Vec::new();
    let mut plain = String::new();
    let mut collector = Collector::default();
    collector.collect(p, &mut words, Some(&mut plain));
    let start = attr(p, None, "begin").and_then(parse_time).or_else(|| words.first().map(|w| w.start_ms))?;
    let stop = attr(p, None, "end").and_then(parse_time).or_else(|| words.last().map(|w| w.end_ms))?;
    let text = if words.is_empty() {
        plain.trim().to_owned()
    } else {
        words.iter().map(|w| w.text.as_str()).collect::<String>().trim().to_owned()
    };
    if text.is_empty() {
        return None;
    }
    let background = if !collector.background.is_empty() {
        let (first, last) = (collector.background[0].start_ms, collector.background.last().expect("слово").end_ms);
        Some(BackingVocals { start_ms: first, end_ms: last, words: trim_last(collector.background) })
    } else if !collector.background_text.is_empty() {
        Some(BackingVocals { start_ms: start, end_ms: stop, words: vec![SyncedWord::new(start, stop, collector.background_text)] })
    } else {
        None
    };
    Some(SyncedLine {
        start_ms: start,
        end_ms: stop,
        text,
        words: trim_last(words),
        agent: attr(p, Some(NS_TTM), "agent").map(str::to_owned),
        side: VocalSide::Start,
        language: attr(p, Some(NS_XML), "lang").map(str::to_owned),
        background,
        translation: collector.translation,
        transliteration: collector.transliteration,
    })
}

/// Время TTML: часы («1:02:03.450», «02:03.45», «3.5») или смещение («12.3s», «450ms», «2m», «1h»).
pub fn parse_time(value: &str) -> Option<i64> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    let number = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite());
    if let Some(v) = text.strip_suffix("ms") {
        return number(v).map(|v| v as i64);
    }
    if let Some(v) = text.strip_suffix('s') {
        return number(v).map(|v| (v * 1000.0) as i64);
    }
    if let Some(v) = text.strip_suffix('m') {
        return number(v).map(|v| (v * 60_000.0) as i64);
    }
    if let Some(v) = text.strip_suffix('h') {
        return number(v).map(|v| (v * 3_600_000.0) as i64);
    }
    let parts: Vec<&str> = text.split(':').collect();
    let seconds = number(parts.last()?)?;
    let minutes: i64 = if parts.len() >= 2 { parts[parts.len() - 2].parse().ok()? } else { 0 };
    let hours: i64 = if parts.len() >= 3 { parts[parts.len() - 3].parse().ok()? } else { 0 };
    Some((hours * 3600 + minutes * 60) * 1000 + (seconds * 1000.0).round() as i64)
}

fn time(ms: i64) -> String {
    let total = ms.max(0);
    let (hours, minutes, seconds, millis) = (total / 3_600_000, total / 60_000 % 60, total / 1000 % 60, total % 1000);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}.{millis:03}")
    } else {
        format!("{minutes:02}:{seconds:02}.{millis:03}")
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn append_words(out: &mut String, words: &[SyncedWord]) {
    for word in words {
        let _ =
            write!(out, "<span begin=\"{}\" end=\"{}\">{}</span>", time(word.start_ms), time(word.end_ms), escape(word.text.trim_end()));
        // Пробел между словами — текстовый узел между span, как пишет Apple.
        if word.text.ends_with(char::is_whitespace) {
            out.push(' ');
        }
    }
}

/// TTML, совместимый с Apple и AMLL.
pub fn write(lyrics: &SyncedLyrics) -> String {
    let mut out = String::new();
    // Дуэт без объявленных исполнителей получает v1 (начало) и v2 (конец).
    let agents: Vec<LyricsAgent> = if !lyrics.agents.is_empty() {
        lyrics.agents.clone()
    } else if lyrics.is_duet() {
        vec![
            LyricsAgent { id: "v1".into(), side: VocalSide::Start, name: None },
            LyricsAgent { id: "v2".into(), side: VocalSide::End, name: None },
        ]
    } else {
        Vec::new()
    };
    let agent_by_side = |side: VocalSide| agents.iter().find(|a| a.side == side).map(|a| a.id.clone());
    let end = lyrics.lines.iter().map(|l| l.end_ms).max().unwrap_or(0);
    let _ = write!(out, "<tt xmlns=\"{NS_TTML}\" xmlns:ttm=\"{NS_TTM}\" xmlns:itunes=\"{NS_ITUNES}\"");
    let _ = write!(out, " itunes:timing=\"{}\"", if lyrics.timing == LyricsTiming::Word { "Word" } else { "Line" });
    if let Some(lang) = &lyrics.language {
        let _ = write!(out, " xml:lang=\"{}\"", escape(lang));
    }
    out.push_str("><head><metadata>");
    for agent in &agents {
        let _ = write!(out, "<ttm:agent type=\"person\" xml:id=\"{}\"", escape(&agent.id));
        match &agent.name {
            None => out.push_str("/>"),
            Some(name) => {
                let _ = write!(out, "><ttm:name type=\"full\">{}</ttm:name></ttm:agent>", escape(name));
            }
        }
    }
    out.push_str("</metadata></head>");
    let first = lyrics.lines.first().map(|l| l.start_ms).unwrap_or(0);
    let _ = write!(out, "<body dur=\"{}\"><div begin=\"{}\" end=\"{}\">", time(end), time(first), time(end));
    for line in &lyrics.lines {
        let _ = write!(out, "<p begin=\"{}\" end=\"{}\"", time(line.start_ms), time(line.end_ms));
        let agent = line.agent.clone().or_else(|| if agents.is_empty() { None } else { agent_by_side(line.side) });
        if let Some(agent) = agent {
            let _ = write!(out, " ttm:agent=\"{}\"", escape(&agent));
        }
        if let Some(lang) = &line.language {
            let _ = write!(out, " xml:lang=\"{}\"", escape(lang));
        }
        out.push('>');
        if line.words.is_empty() {
            out.push_str(&escape(&line.text));
        } else {
            append_words(&mut out, &line.words);
        }
        if let Some(background) = &line.background {
            out.push_str("<span ttm:role=\"x-bg\">");
            append_words(&mut out, &background.words);
            out.push_str("</span>");
        }
        if let Some(translation) = &line.translation {
            let _ = write!(out, "<span ttm:role=\"x-translation\">{}</span>", escape(translation));
        }
        if let Some(roman) = &line.transliteration {
            let _ = write!(out, "<span ttm:role=\"x-roman\">{}</span>", escape(roman));
        }
        out.push_str("</p>");
    }
    out.push_str("</div></body></tt>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_through_ttml() {
        let spec: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../spec/lyrics.vectors.json")).unwrap())
                .unwrap();
        for case in spec["cases"].as_array().unwrap() {
            if let Some(lyrics) = super::super::parse_synced(case["input"].as_str().unwrap()) {
                let written = write(&lyrics);
                let again = parse(&written).unwrap_or_else(|| panic!("{} не читается обратно: {written}", case["id"]));
                assert_eq!(again.lines, lyrics.lines, "{}", case["id"]);
            }
        }
        assert_eq!(parse_time("1:02:03.450"), Some(3_723_450));
        assert_eq!(parse_time("450ms"), Some(450));
        assert_eq!(parse_time("1.5s"), Some(1500));
        assert_eq!(parse_time("x"), None);
    }

    #[test]
    fn doctype_is_refused() {
        let evil = r#"<?xml version="1.0"?><!DOCTYPE tt [<!ENTITY x SYSTEM "file:///etc/passwd">]><tt xmlns="http://www.w3.org/ns/ttml"><body><div><p begin="1s" end="2s">&x;</p></div></body></tt>"#;
        assert_eq!(parse(evil), None);
    }
}
