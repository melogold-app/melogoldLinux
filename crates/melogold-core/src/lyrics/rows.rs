//! Строки экрана синхронного текста (Android `LyricsModel.kt`, Windows `LyricRows.cs`): спетые строки
//! и проигрыши — перед первой строкой и в паузах от 4 с.

use super::{SyncedLine, SyncedLyrics, VocalSide};

/// Пауза не короче этой перед строкой или между строками показывается проигрышем.
pub const INTERLUDE_MIN_GAP_MS: i64 = 4_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LyricRow {
    Sung(SyncedLine),
    /// Проигрыш (три точки); `side` — сторона строки после паузы: туда смотрят дальше.
    Interlude {
        start_ms: i64,
        end_ms: i64,
        side: VocalSide,
    },
}

impl LyricRow {
    pub fn start_ms(&self) -> i64 {
        match self {
            LyricRow::Sung(line) => line.start_ms,
            LyricRow::Interlude { start_ms, .. } => *start_ms,
        }
    }

    pub fn end_ms(&self) -> i64 {
        match self {
            LyricRow::Sung(line) => line.end_ms,
            LyricRow::Interlude { end_ms, .. } => *end_ms,
        }
    }
}

fn is_filler(text: &str) -> bool {
    text.trim().chars().all(|c| c.is_whitespace() || "♪♫♬♩….".contains(c))
}

/// Спетые строки и проигрыши; строки-заполнители («♪», «…») становятся проигрышем или исчезают.
pub fn build(lyrics: &SyncedLyrics) -> Vec<LyricRow> {
    let mut sung: Vec<&SyncedLine> = lyrics.lines.iter().filter(|l| !is_filler(&l.text)).collect();
    sung.sort_by_key(|l| l.start_ms);
    let mut rows = Vec::new();
    let Some(first) = sung.first() else { return rows };
    if first.start_ms >= INTERLUDE_MIN_GAP_MS {
        rows.push(LyricRow::Interlude { start_ms: 0, end_ms: first.start_ms, side: first.side });
    }
    for (index, line) in sung.iter().enumerate() {
        rows.push(LyricRow::Sung((*line).clone()));
        let Some(next) = sung.get(index + 1) else { continue };
        // Строка-заполнитель между ними заканчивает спетую там, где начинается.
        let filler = lyrics.lines.iter().find(|l| l.start_ms > line.start_ms && l.start_ms < next.start_ms && is_filler(&l.text));
        let gap_start = filler.map(|f| f.start_ms.min(line.end_ms)).unwrap_or(line.end_ms);
        if next.start_ms - gap_start >= INTERLUDE_MIN_GAP_MS {
            rows.push(LyricRow::Interlude { start_ms: gap_start, end_ms: next.start_ms, side: next.side });
        }
    }
    rows
}

/// Индекс последней строки с началом не позже `position_ms`; `None` до первой.
pub fn active_index_at(rows: &[LyricRow], position_ms: i64) -> Option<usize> {
    let count = rows.partition_point(|r| r.start_ms() <= position_ms);
    count.checked_sub(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interludes_before_first_line_and_in_long_gaps() {
        let lyrics = super::super::lrc::parse("[00:05.00]One\n[00:07.00]\n[00:12.00]Two\n[00:14.00]♪\n[00:15.00]Three\n").unwrap();
        let rows = build(&lyrics);
        let kinds: Vec<(&str, i64)> = rows
            .iter()
            .map(|r| match r {
                LyricRow::Sung(l) => (l.text.as_str(), l.start_ms),
                LyricRow::Interlude { start_ms, .. } => ("…", *start_ms),
            })
            .collect();
        assert_eq!(kinds, [("…", 0), ("One", 5000), ("…", 7000), ("Two", 12_000), ("Three", 15_000)]);
        assert_eq!(active_index_at(&rows, 0), Some(0));
        assert_eq!(active_index_at(&rows, 12_500), Some(3));
        assert_eq!(active_index_at(&[], 5), None);
    }
}
