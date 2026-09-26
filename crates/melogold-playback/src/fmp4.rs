//! Разбор фрагментированного MP4 (DASH), в котором YouTube отдаёт AAC (itag 140/139), — порт
//! Windows `FragmentedMp4.cs`.
//!
//! Зачем свой разбор, а не `qtdemux` (грабли §9 п. 4): в режиме pull `qtdemux` при перемотке не
//! пользуется `sidx`, а читает заголовки всех фрагментов от начала по очереди — по сети это десяток
//! последовательных запросов на одну перемотку. Здесь перемотка — один запрос к нужному фрагменту.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct FormatError(pub &'static str);

/// Звуковая дорожка из `moov`.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTrack {
    pub timescale: u32,
    pub audio_specific_config: Vec<u8>,
    pub object_type: u8,
    pub sample_rate: u32,
    pub channels: u32,
    pub average_bitrate: u32,
    pub default_sample_duration: u32,
    pub default_sample_size: u32,
    pub duration_ticks: u64,
}

/// Фрагмент из `sidx`: где лежит (`moof`+`mdat`) и какое время покрывает (в тиках индекса).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub index: usize,
    pub offset: u64,
    pub size: u32,
    pub start_ticks: u64,
    pub duration_ticks: u64,
}

/// Кадр AAC: где он в данных фрагмента, когда и сколько звучит (в тиках дорожки).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub offset: usize,
    pub size: usize,
    pub time_ticks: u64,
    pub duration_ticks: u64,
}

/// Начало файла: дорожка и таблица фрагментов.
#[derive(Clone, Debug, PartialEq)]
pub struct Index {
    pub track: AudioTrack,
    pub fragments: Vec<Fragment>,
    pub index_timescale: u32,
    /// Где кончаются заголовки (`ftyp`, `moov`, `sidx`) — начало первого фрагмента.
    pub header_end: u64,
}

impl Index {
    pub fn duration(&self) -> Duration {
        match self.fragments.last() {
            Some(last) if self.index_timescale > 0 => {
                Duration::from_secs_f64((last.start_ticks + last.duration_ticks) as f64 / f64::from(self.index_timescale))
            }
            _ => Duration::from_secs_f64(self.track.duration_ticks as f64 / f64::from(self.track.timescale.max(1))),
        }
    }

    /// Фрагмент, в котором звучит `position`.
    pub fn fragment_at(&self, position: Duration) -> usize {
        let ticks = (position.as_secs_f64() * f64::from(self.index_timescale)) as u64;
        self.fragments.iter().position(|f| ticks < f.start_ticks + f.duration_ticks).unwrap_or(self.fragments.len().saturating_sub(1))
    }

    /// Полная длина файла по таблице фрагментов.
    pub fn total_size(&self) -> u64 {
        self.fragments.last().map(|f| f.offset + u64::from(f.size)).unwrap_or(self.header_end)
    }
}

pub const SAMPLE_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

fn u16_at(d: &[u8], o: usize) -> Result<u16, FormatError> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or(FormatError("truncated"))
}

fn u32_at(d: &[u8], o: usize) -> Result<u32, FormatError> {
    d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or(FormatError("truncated"))
}

fn u64_at(d: &[u8], o: usize) -> Result<u64, FormatError> {
    d.get(o..o + 8).map(|b| u64::from_be_bytes(b.try_into().expect("8 байт"))).ok_or(FormatError("truncated"))
}

/// Бокс: тип, начало, начало содержимого, конец (может выходить за доступные данные).
#[derive(Clone, Copy, Debug)]
struct BoxRef {
    kind: [u8; 4],
    start: usize,
    content: usize,
    end: usize,
}

/// Боксы на отрезке `[start, end)`; последний может быть обрезан (его `end` > `end` отрезка).
fn boxes(d: &[u8], start: usize, end: usize) -> Vec<BoxRef> {
    let mut found = Vec::new();
    let mut i = start;
    while i + 8 <= end {
        let Ok(size32) = u32_at(d, i) else { break };
        let kind = [d[i + 4], d[i + 5], d[i + 6], d[i + 7]];
        let (size, header) = match size32 {
            1 => match u64_at(d, i + 8) {
                Ok(size) if i + 16 <= end => (size, 16),
                _ => break,
            },
            0 => ((end - i) as u64, 8),
            n => (u64::from(n), 8),
        };
        if size < header as u64 {
            break;
        }
        let box_end = i.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
        found.push(BoxRef { kind, start: i, content: i + header, end: box_end });
        i = box_end;
    }
    found
}

fn child(d: &[u8], start: usize, end: usize, kind: &[u8; 4]) -> Option<BoxRef> {
    boxes(d, start, end.min(d.len())).into_iter().find(|b| &b.kind == kind && b.end <= d.len())
}

/// Разбирает начало файла (`ftyp`, `moov`, `sidx`). Ошибка `truncated` — прочитать больше.
pub fn parse_index(head: &[u8]) -> Result<Index, FormatError> {
    let mut track = None;
    let mut fragments = None;
    for b in boxes(head, 0, head.len()) {
        if &b.kind == b"moof" || &b.kind == b"mdat" {
            break;
        }
        if b.end > head.len() {
            return Err(FormatError("truncated"));
        }
        match &b.kind {
            b"moov" => track = Some(parse_moov(head, b.content, b.end)?),
            b"sidx" => fragments = Some((parse_sidx(head, b.content, b.end)?, b.end)),
            _ => {}
        }
        if track.is_some() && fragments.is_some() {
            break;
        }
    }
    let track = track.ok_or(FormatError("no moov"))?;
    let ((index_timescale, fragments), header_end) = fragments.ok_or(FormatError("no sidx"))?;
    Ok(Index { track, fragments, index_timescale, header_end: header_end as u64 })
}

fn parse_moov(d: &[u8], start: usize, end: usize) -> Result<AudioTrack, FormatError> {
    let (mut timescale, mut default_duration, mut default_size, mut duration_ticks) = (0u32, 1024u32, 0u32, 0u64);
    let (mut asc, mut channels, mut sample_rate, mut bitrate) = (None, 2u32, 44100u32, 128_000u32);
    for b in boxes(d, start, end) {
        if &b.kind == b"mvex" {
            if let Some(trex) = child(d, b.content, b.end, b"trex") {
                // trex: версия/флаги(4) trackID(4) description(4) duration(4) size(4)
                default_duration = u32_at(d, trex.content + 12)?;
                default_size = u32_at(d, trex.content + 16)?;
            }
        }
        if &b.kind != b"trak" {
            continue;
        }
        let mdia = child(d, b.content, b.end, b"mdia").ok_or(FormatError("no mdia"))?;
        if let Some(mdhd) = child(d, mdia.content, mdia.end, b"mdhd") {
            let version = d[mdhd.content];
            timescale = if version == 1 { u32_at(d, mdhd.content + 20)? } else { u32_at(d, mdhd.content + 12)? };
            duration_ticks = if version == 1 { u64_at(d, mdhd.content + 24)? } else { u64::from(u32_at(d, mdhd.content + 16)?) };
        }
        let minf = child(d, mdia.content, mdia.end, b"minf").ok_or(FormatError("no minf"))?;
        let stbl = child(d, minf.content, minf.end, b"stbl").ok_or(FormatError("no stbl"))?;
        let stsd = child(d, stbl.content, stbl.end, b"stsd").ok_or(FormatError("no stsd"))?;
        // stsd: версия/флаги(4) число(4), затем записи.
        for entry in boxes(d, stsd.content + 8, stsd.end) {
            if &entry.kind != b"mp4a" {
                continue;
            }
            // SampleEntry: 6 резерв + 2 индекс; AudioSampleEntry: 8 резерв, channels(2), size(2), 4 резерв, rate 16.16(4).
            let p = entry.content + 8;
            channels = u32::from(u16_at(d, p + 8)?);
            sample_rate = u32_at(d, p + 16)? >> 16;
            if let Some(esds) = child(d, p + 20, entry.end, b"esds") {
                let (found, rate) = parse_esds(d, esds.content + 4, esds.end, bitrate);
                asc = found;
                bitrate = rate;
            }
        }
    }
    let asc = asc.filter(|a| a.len() >= 2).ok_or(FormatError("no AudioSpecificConfig"))?;
    let object_type = asc[0] >> 3;
    let frequency_index = usize::from(((asc[0] & 0x07) << 1) | (asc[1] >> 7));
    let channel_config = u32::from((asc[1] >> 3) & 0x0F);
    if channel_config > 0 {
        channels = channel_config;
    }
    if let Some(rate) = SAMPLE_RATES.get(frequency_index) {
        sample_rate = *rate;
    }
    Ok(AudioTrack {
        timescale: if timescale == 0 { sample_rate } else { timescale },
        audio_specific_config: asc,
        object_type,
        sample_rate,
        channels,
        average_bitrate: bitrate,
        default_sample_duration: default_duration,
        default_sample_size: default_size,
        duration_ticks,
    })
}

/// `esds`: дескрипторы ES → DecoderConfig → DecoderSpecificInfo (AudioSpecificConfig).
fn parse_esds(d: &[u8], p: usize, end: usize, bitrate: u32) -> (Option<Vec<u8>>, u32) {
    let descriptor = |at: usize| -> Option<(u8, usize, usize)> {
        let tag = *d.get(at)?;
        let (mut size, mut i) = (0usize, at + 1);
        for _ in 0..4 {
            let b = *d.get(i)?;
            i += 1;
            size = (size << 7) | usize::from(b & 0x7F);
            if b & 0x80 == 0 {
                break;
            }
        }
        Some((tag, size, i))
    };
    let parse = || -> Option<(Vec<u8>, u32)> {
        let (tag, _, content) = descriptor(p)?;
        if tag != 0x03 {
            return None;
        }
        let mut q = content + 2;
        let flags = *d.get(q)?;
        q += 1;
        if flags & 0x80 != 0 {
            q += 2;
        }
        if flags & 0x40 != 0 {
            q += 1 + usize::from(*d.get(q)?);
        }
        if flags & 0x20 != 0 {
            q += 2;
        }
        let (tag, _, config) = descriptor(q)?;
        if tag != 0x04 {
            return None;
        }
        let average = u32_at(d, config + 9).ok()?;
        let (tag, size, info) = descriptor(config + 13)?;
        if tag != 0x05 || info + size > end {
            return None;
        }
        Some((d.get(info..info + size)?.to_vec(), if average > 0 { average } else { bitrate }))
    };
    match parse() {
        Some((asc, rate)) => (Some(asc), rate),
        None => (None, bitrate),
    }
}

fn parse_sidx(d: &[u8], p: usize, end: usize) -> Result<(u32, Vec<Fragment>), FormatError> {
    let version = d[p];
    let timescale = u32_at(d, p + 8)?;
    let (earliest, first_offset, mut q) = if version == 0 {
        (u64::from(u32_at(d, p + 12)?), u64::from(u32_at(d, p + 16)?), p + 20)
    } else {
        (u64_at(d, p + 12)?, u64_at(d, p + 20)?, p + 28)
    };
    let count = usize::from(u16_at(d, q + 2)?);
    q += 4;
    let mut fragments = Vec::with_capacity(count);
    let (mut offset, mut time) = (end as u64 + first_offset, earliest);
    for index in 0..count {
        let size = u32_at(d, q)? & 0x7FFF_FFFF;
        let duration = u64::from(u32_at(d, q + 4)?);
        fragments.push(Fragment { index, offset, size, start_ticks: time, duration_ticks: duration });
        offset += u64::from(size);
        time += duration;
        q += 12;
    }
    Ok((timescale, fragments))
}

/// Кадры фрагмента: `data` — байты `moof`+`mdat` (длиной во весь фрагмент), из них доступны первые
/// `available`. `moof` должен быть доступен целиком; кадры могут выходить за доступное — их
/// размеры известны из `trun`.
pub fn parse_fragment(data: &[u8], available: usize, track: &AudioTrack) -> Result<Vec<Sample>, FormatError> {
    let mut samples = Vec::new();
    let available = available.min(data.len());
    for moof in boxes(data, 0, available).into_iter().filter(|b| &b.kind == b"moof" && b.end <= available) {
        for traf in boxes(data, moof.content, moof.end).into_iter().filter(|b| &b.kind == b"traf") {
            let (mut default_duration, mut default_size) = (track.default_sample_duration, track.default_sample_size);
            let (mut base_offset, mut time) = (moof.start as u64, 0u64);
            for b in boxes(data, traf.content, traf.end) {
                match &b.kind {
                    b"tfhd" => {
                        let flags = u32_at(data, b.content)? & 0xFF_FFFF;
                        let mut q = b.content + 8;
                        if flags & 0x01 != 0 {
                            base_offset = u64_at(data, q)?;
                            q += 8;
                        }
                        if flags & 0x02 != 0 {
                            q += 4;
                        }
                        if flags & 0x08 != 0 {
                            default_duration = u32_at(data, q)?;
                            q += 4;
                        }
                        if flags & 0x10 != 0 {
                            default_size = u32_at(data, q)?;
                        }
                    }
                    b"tfdt" => {
                        time = if data[b.content] == 1 { u64_at(data, b.content + 4)? } else { u64::from(u32_at(data, b.content + 4)?) };
                    }
                    b"trun" => {
                        let flags = u32_at(data, b.content)? & 0xFF_FFFF;
                        let count = u32_at(data, b.content + 4)? as usize;
                        let mut q = b.content + 8;
                        let mut data_offset = 0i64;
                        if flags & 0x01 != 0 {
                            data_offset = i64::from(u32_at(data, q)? as i32);
                            q += 4;
                        }
                        if flags & 0x04 != 0 {
                            q += 4;
                        }
                        let mut position = usize::try_from(base_offset as i64 + data_offset).map_err(|_| FormatError("bad data offset"))?;
                        for _ in 0..count {
                            let (mut duration, mut size) = (default_duration, default_size);
                            if flags & 0x100 != 0 {
                                duration = u32_at(data, q)?;
                                q += 4;
                            }
                            if flags & 0x200 != 0 {
                                size = u32_at(data, q)?;
                                q += 4;
                            }
                            if flags & 0x400 != 0 {
                                q += 4;
                            }
                            if flags & 0x800 != 0 {
                                q += 4;
                            }
                            let size = size as usize;
                            if position + size > data.len() {
                                return Err(FormatError("sample outside the fragment"));
                            }
                            samples.push(Sample { offset: position, size, time_ticks: time, duration_ticks: u64::from(duration) });
                            position += size;
                            time += u64::from(duration);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(samples)
}

/// Где кончается `moof` фрагмента, если его начало уже есть: столько байт нужно, чтобы знать кадры.
pub fn moof_end(data: &[u8]) -> Option<usize> {
    boxes(data, 0, data.len()).into_iter().find(|b| &b.kind == b"moof").map(|b| b.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Первые 2835 байт настоящего потока itag 140 (dQw4w9WgXcQ): `ftyp`, `moov`, `sidx`, первый
    /// `moof` — без звука.
    const HEAD: &[u8] = include_bytes!("../tests/fixtures/itag140-head.bin");

    #[test]
    fn index_of_a_real_stream() {
        let index = parse_index(HEAD).unwrap();
        assert_eq!(index.track.sample_rate, 44100);
        assert_eq!(index.track.channels, 2);
        assert_eq!(index.track.object_type, 2, "AAC-LC");
        // YouTube кладёт в DecoderSpecificInfo 16 байт: ASC «12 10» и нули; qtdemux отдаёт декодеру то же.
        assert_eq!(index.track.audio_specific_config[..2], [0x12, 0x10]);
        assert_eq!(index.fragments.len(), 22);
        assert_eq!(index.header_end, 1019);
        assert_eq!(index.fragments[0].offset, 1019);
        assert_eq!(index.total_size(), 3_449_447, "как contentLength формата");
        let seconds = index.duration().as_secs_f64();
        assert!((seconds - 213.09).abs() < 0.05, "{seconds}");
        assert_eq!(index.fragment_at(Duration::ZERO), 0);
        assert_eq!(index.fragment_at(Duration::from_secs(150)), 15);
        assert_eq!(index.fragment_at(Duration::from_secs(10_000)), 21);
    }

    #[test]
    fn truncated_head_asks_for_more() {
        assert_eq!(parse_index(&HEAD[..500]), Err(FormatError("truncated")));
    }

    #[test]
    fn samples_of_the_first_fragment() {
        let index = parse_index(HEAD).unwrap();
        let first = index.fragments[0];
        // Байты фрагмента: есть только moof, mdat ещё не пришёл — кадры всё равно известны.
        let mut data = vec![0u8; first.size as usize];
        let have = HEAD.len() - first.offset as usize;
        data[..have].copy_from_slice(&HEAD[first.offset as usize..]);
        assert_eq!(moof_end(&data[..have]), Some(1816));
        let samples = parse_fragment(&data, have, &index.track).unwrap();
        assert!(samples.len() > 400, "{}", samples.len());
        assert_eq!(samples[0].offset, 1816 + 8, "первый кадр — сразу за заголовком mdat");
        assert_eq!(samples[0].time_ticks, 0);
        assert!(samples.iter().all(|s| s.duration_ticks == 1024));
        let last = samples.last().unwrap();
        assert_eq!(last.offset + last.size, first.size as usize, "кадры заполняют mdat до конца");
        assert!(parse_fragment(&data, 100, &index.track).unwrap().is_empty(), "без целого moof кадров нет");
    }
}
