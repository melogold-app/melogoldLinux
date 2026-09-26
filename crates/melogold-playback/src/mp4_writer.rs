//! «Сохранить файлом» (Android `FileExport` + `Mp4Tags`, Windows `Mp4Writer.cs`): фрагментированный
//! MP4 (DASH), в котором YouTube отдаёт AAC, превращается в обычный .m4a без перекодирования — те же
//! кадры AAC одним `mdat`, `moov` в начале (файл играет сразу, ещё не дочитанный), теги название,
//! исполнитель, альбом и обложка.

use crate::fmp4::{self, AudioTrack, FormatError};

/// Что файл говорит о себе: теги iTunes (`moov/udta/meta/ilst`), которые читают все плееры.
#[derive(Clone, Debug, Default)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// JPEG или PNG.
    pub cover: Option<Vec<u8>>,
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

/// Файл целиком (`fragmented` — все байты потока) → .m4a с тегами.
pub fn from_fragmented(fragmented: &[u8], tags: &Tags) -> Result<Vec<u8>, FormatError> {
    let index = fmp4::parse_index(fragmented)?;
    let track = &index.track;
    let (mut sizes, mut durations, mut chunks) = (Vec::new(), Vec::new(), Vec::new());
    let mut media = Vec::with_capacity(fragmented.len());
    for fragment in &index.fragments {
        let start = usize::try_from(fragment.offset).map_err(|_| FormatError("the file is too large"))?;
        let end = start + fragment.size as usize;
        let data = fragmented.get(start..end).ok_or(FormatError("the stream is not complete"))?;
        let samples = fmp4::parse_fragment(data, data.len(), track)?;
        if samples.is_empty() {
            continue;
        }
        // Фрагмент — один кусок (chunk): его кадры лежат подряд.
        chunks.push((media.len() as u64, samples.len() as u32));
        for sample in samples {
            let bytes = data.get(sample.offset..sample.offset + sample.size).ok_or(FormatError("the stream is not complete"))?;
            media.extend_from_slice(bytes);
            sizes.push(sample.size as u32);
            durations.push(sample.duration_ticks as u32);
        }
    }
    if sizes.is_empty() {
        return Err(FormatError("no audio frames"));
    }
    let ftyp = ftyp();
    // moov с условным началом mdat — только чтобы узнать его длину: смещения в stco длины не меняют.
    let probe = moov(track, &sizes, &durations, &chunks, 0, tags);
    let media_start = (ftyp.len() + probe.len() + 8) as u64;
    if media_start + media.len() as u64 > u64::from(u32::MAX) {
        return Err(FormatError("the file is too large"));
    }
    let moov = moov(track, &sizes, &durations, &chunks, media_start, tags);
    let mut output = Vec::with_capacity(media_start as usize + media.len());
    output.extend_from_slice(&ftyp);
    output.extend_from_slice(&moov);
    output.extend_from_slice(&((media.len() + 8) as u32).to_be_bytes());
    output.extend_from_slice(b"mdat");
    output.extend_from_slice(&media);
    Ok(output)
}

fn ftyp() -> Vec<u8> {
    let mut w = BoxWriter::default();
    w.begin(b"ftyp");
    w.bytes(b"M4A ");
    w.u32(0x200);
    for brand in [b"M4A ", b"mp42", b"isom", b"iso2"] {
        w.bytes(brand);
    }
    w.end();
    w.data
}

fn moov(track: &AudioTrack, sizes: &[u32], durations: &[u32], chunks: &[(u64, u32)], media_start: u64, tags: &Tags) -> Vec<u8> {
    let duration = durations.iter().map(|d| u64::from(*d)).sum::<u64>().min(u64::from(u32::MAX)) as u32;
    let mut w = BoxWriter::default();
    w.begin(b"moov");

    w.begin_full(b"mvhd", 0, 0);
    w.u32(0);
    w.u32(0);
    w.u32(track.timescale);
    w.u32(duration);
    w.u32(0x0001_0000);
    w.u16(0x0100);
    w.zeros(10);
    MATRIX.iter().for_each(|v| w.u32(*v));
    w.zeros(24);
    w.u32(2);
    w.end();

    w.begin(b"trak");
    w.begin_full(b"tkhd", 0, 0x000003);
    w.u32(0);
    w.u32(0);
    w.u32(1);
    w.u32(0);
    w.u32(duration);
    w.zeros(8);
    w.u16(0);
    w.u16(0);
    w.u16(0x0100);
    w.u16(0);
    MATRIX.iter().for_each(|v| w.u32(*v));
    w.u32(0);
    w.u32(0);
    w.end();

    w.begin(b"mdia");
    w.begin_full(b"mdhd", 0, 0);
    w.u32(0);
    w.u32(0);
    w.u32(track.timescale);
    w.u32(duration);
    w.u16(0x55C4); // «und»
    w.u16(0);
    w.end();
    w.begin_full(b"hdlr", 0, 0);
    w.u32(0);
    w.bytes(b"soun");
    w.zeros(12);
    w.bytes(b"SoundHandler\0");
    w.end();

    w.begin(b"minf");
    w.begin_full(b"smhd", 0, 0);
    w.u32(0);
    w.end();
    w.begin(b"dinf");
    w.begin_full(b"dref", 0, 0);
    w.u32(1);
    w.begin_full(b"url ", 0, 1);
    w.end();
    w.end();
    w.end();

    w.begin(b"stbl");
    w.begin_full(b"stsd", 0, 0);
    w.u32(1);
    w.begin(b"mp4a");
    w.zeros(6);
    w.u16(1);
    w.zeros(8);
    w.u16(track.channels.clamp(1, 8) as u16);
    w.u16(16);
    w.u16(0);
    w.u16(0);
    w.u32(track.sample_rate.min(0xFFFF) << 16);
    esds(&mut w, track);
    w.end();
    w.end();

    // stts: одинаковые длительности подряд — одной записью.
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &delta in durations {
        match runs.last_mut() {
            Some((count, last)) if *last == delta => *count += 1,
            _ => runs.push((1, delta)),
        }
    }
    w.begin_full(b"stts", 0, 0);
    w.u32(runs.len() as u32);
    for (count, delta) in runs {
        w.u32(count);
        w.u32(delta);
    }
    w.end();

    // stsc: сколько кадров в куске — меняется только на последнем фрагменте.
    let mut stsc: Vec<(u32, u32)> = Vec::new();
    for (i, (_, samples)) in chunks.iter().enumerate() {
        if stsc.last().is_none_or(|(_, last)| last != samples) {
            stsc.push((i as u32 + 1, *samples));
        }
    }
    w.begin_full(b"stsc", 0, 0);
    w.u32(stsc.len() as u32);
    for (first, samples) in stsc {
        w.u32(first);
        w.u32(samples);
        w.u32(1);
    }
    w.end();

    w.begin_full(b"stsz", 0, 0);
    w.u32(0);
    w.u32(sizes.len() as u32);
    sizes.iter().for_each(|s| w.u32(*s));
    w.end();

    w.begin_full(b"stco", 0, 0);
    w.u32(chunks.len() as u32);
    for (offset, _) in chunks {
        w.u32((media_start + offset) as u32);
    }
    w.end();

    w.end(); // stbl
    w.end(); // minf
    w.end(); // mdia
    w.end(); // trak

    udta(&mut w, tags);
    w.end(); // moov
    w.data
}

/// `esds`: ES → DecoderConfig (AAC, звук) → DecoderSpecificInfo (AudioSpecificConfig) → SLConfig.
fn esds(w: &mut BoxWriter, track: &AudioTrack) {
    let asc = &track.audio_specific_config;
    let decoder_config_size = 13 + 2 + asc.len();
    let es_size = 3 + 2 + decoder_config_size + 2 + 1;
    w.begin_full(b"esds", 0, 0);
    w.u8(0x03);
    w.u8(es_size as u8);
    w.u16(1);
    w.u8(0);
    w.u8(0x04);
    w.u8(decoder_config_size as u8);
    w.u8(0x40); // MPEG-4 Audio
    w.u8(0x15); // звук
    w.u24(0);
    w.u32(track.average_bitrate);
    w.u32(track.average_bitrate);
    w.u8(0x05);
    w.u8(asc.len() as u8);
    w.bytes(asc);
    w.u8(0x06);
    w.u8(1);
    w.u8(2);
    w.end();
}

/// `udta/meta/ilst`: ©nam, ©ART, ©alb и covr — как их пишет iTunes.
fn udta(w: &mut BoxWriter, tags: &Tags) {
    let mut items: Vec<([u8; 4], u32, Vec<u8>)> = Vec::new();
    let mut text = |name: &[u8; 3], value: Option<&str>| {
        if let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) {
            items.push(([0xA9, name[0], name[1], name[2]], 1, value.as_bytes().to_vec()));
        }
    };
    text(b"nam", tags.title.as_deref());
    text(b"ART", tags.artist.as_deref());
    text(b"alb", tags.album.as_deref());
    text(b"too", Some("Melogold"));
    if let Some(cover) = tags.cover.as_ref().filter(|c| c.len() > 4) {
        let kind = if cover.starts_with(&[0x89, 0x50]) { 14 } else { 13 };
        items.push((*b"covr", kind, cover.clone()));
    }
    w.begin(b"udta");
    w.begin_full(b"meta", 0, 0);
    w.begin_full(b"hdlr", 0, 0);
    w.u32(0);
    w.bytes(b"mdir");
    w.bytes(b"appl");
    w.zeros(9);
    w.end();
    w.begin(b"ilst");
    for (kind, data_kind, data) in items {
        w.begin(&kind);
        w.begin(b"data");
        w.u32(data_kind);
        w.u32(0);
        w.bytes(&data);
        w.end();
        w.end();
    }
    w.end();
    w.end();
    w.end();
}

/// Боксы MP4 по порядку: размер пишется, когда бокс закрыт.
#[derive(Default)]
struct BoxWriter {
    data: Vec<u8>,
    open: Vec<usize>,
}

impl BoxWriter {
    fn begin(&mut self, kind: &[u8; 4]) {
        self.open.push(self.data.len());
        self.u32(0);
        self.bytes(kind);
    }

    fn begin_full(&mut self, kind: &[u8; 4], version: u8, flags: u32) {
        self.begin(kind);
        self.u8(version);
        self.u24(flags);
    }

    fn end(&mut self) {
        let start = self.open.pop().expect("бокс открыт");
        let size = (self.data.len() - start) as u32;
        self.data[start..start + 4].copy_from_slice(&size.to_be_bytes());
    }

    fn u8(&mut self, value: u8) {
        self.data.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    fn u24(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_be_bytes()[1..]);
    }

    fn u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }

    fn zeros(&mut self, count: usize) {
        self.data.resize(self.data.len() + count, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32b(v: u32) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    fn u16b(v: u16) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    fn mp4_box(kind: &str, parts: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = parts.concat();
        [u32b(body.len() as u32 + 8), kind.as_bytes().to_vec(), body].concat()
    }

    fn full(kind: &str, flags: u32, parts: &[Vec<u8>]) -> Vec<u8> {
        let mut all = vec![u32b(flags)];
        all.extend_from_slice(parts);
        mp4_box(kind, &all)
    }

    /// Кадр AAC: байты по номеру, чтобы в выходе было видно, какой кадр где.
    fn frame(number: usize, size: usize) -> Vec<u8> {
        (0..size).map(|i| (number * 7 + i) as u8).collect()
    }

    /// fMP4 (как у YouTube) с `fragments` фрагментами по `per_fragment` кадров (последний — короче).
    fn fragmented(fragments: usize, per_fragment: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let asc = vec![0x12, 0x10]; // AAC LC, 44100, 2 канала
        let esds = full(
            "esds",
            0,
            &[
                vec![0x03, (3 + 2 + 13 + 2 + asc.len() + 2 + 1) as u8, 0x00, 0x01, 0x00],
                vec![0x04, (13 + 2 + asc.len()) as u8, 0x40, 0x15, 0, 0, 0],
                u32b(128_000),
                u32b(128_000),
                vec![0x05, asc.len() as u8],
                asc.clone(),
                vec![0x06, 0x01, 0x02],
            ],
        );
        let mp4a = mp4_box("mp4a", &[vec![0; 6], u16b(1), vec![0; 8], u16b(2), u16b(16), u16b(0), u16b(0), u32b(44100 << 16), esds]);
        let stbl = mp4_box("stbl", &[full("stsd", 0, &[u32b(1), mp4a])]);
        let mdhd = full("mdhd", 0, &[u32b(0), u32b(0), u32b(44100), u32b(0), u16b(0x55C4), u16b(0)]);
        let trak = mp4_box("trak", &[mp4_box("mdia", &[mdhd, mp4_box("minf", &[stbl])])]);
        let mvex = mp4_box("mvex", &[full("trex", 0, &[u32b(1), u32b(1), u32b(1024), u32b(0), u32b(0)])]);
        let moov = mp4_box("moov", &[trak, mvex]);
        let ftyp = mp4_box("ftyp", &[b"dash".to_vec(), u32b(0)]);

        let (mut frames, mut chunks, mut number) = (Vec::new(), Vec::new(), 0);
        for f in 0..fragments {
            let count = if f == fragments - 1 { (per_fragment / 2).max(1) } else { per_fragment };
            let samples: Vec<Vec<u8>> = (0..count)
                .map(|_| {
                    number += 1;
                    frame(number, 20 + number % 5)
                })
                .collect();
            frames.extend(samples.iter().cloned());
            let trun = |data_offset: u32| {
                let mut parts = vec![u32b(count as u32), u32b(data_offset)];
                for s in &samples {
                    parts.push(u32b(1024));
                    parts.push(u32b(s.len() as u32));
                }
                full("trun", 0x000301, &parts)
            };
            // Смещение данных — от начала moof: сначала moof с условным смещением, чтобы узнать его длину.
            let moof = |data_offset: u32| {
                mp4_box(
                    "moof",
                    &[
                        full("mfhd", 0, &[u32b(f as u32 + 1)]),
                        mp4_box(
                            "traf",
                            &[full("tfhd", 0, &[u32b(1)]), full("tfdt", 0, &[u32b((f * per_fragment * 1024) as u32)]), trun(data_offset)],
                        ),
                    ],
                )
            };
            let moof_length = moof(0).len() as u32;
            chunks.push([moof(moof_length + 8), mp4_box("mdat", &[samples.concat()])].concat());
        }
        let mut references = Vec::new();
        for chunk in &chunks {
            references.extend(u32b(chunk.len() as u32));
            references.extend(u32b(1024 * per_fragment as u32));
            references.extend(u32b(0x9000_0000));
        }
        let sidx = full("sidx", 0, &[u32b(1), u32b(44100), u32b(0), u32b(0), u16b(0), u16b(chunks.len() as u16), references]);
        ([ftyp, moov, sidx, chunks.concat()].concat(), frames)
    }

    /// Бокс по пути вида «moov/trak/mdia/minf/stbl/stsz»: (начало содержимого, конец).
    fn find(d: &[u8], path: &str) -> (usize, usize) {
        let (mut start, mut end) = (0, d.len());
        for name in path.split('/') {
            let mut i = start;
            loop {
                assert!(i + 8 <= end, "нет бокса {name} в {path}");
                let size = u32::from_be_bytes(d[i..i + 4].try_into().unwrap()) as usize;
                if &d[i + 4..i + 8] == name.as_bytes() {
                    // Полные боксы с детьми: у meta сначала версия и флаги.
                    let skip = if name == "meta" { 12 } else { 8 };
                    start = i + skip;
                    end = i + size;
                    break;
                }
                i += size;
            }
        }
        (start, end)
    }

    fn be(d: &[u8], o: usize) -> u32 {
        u32::from_be_bytes(d[o..o + 4].try_into().unwrap())
    }

    #[test]
    fn frames_move_into_one_mdat_in_order() {
        let (file, frames) = fragmented(4, 10);
        let tags = Tags {
            title: Some("Песня".into()), artist: Some("Исполнитель".into()), album: Some("Альбом".into()), cover: None
        };
        let out = from_fragmented(&file, &tags).expect("m4a");
        assert_eq!(&out[4..12], b"ftypM4A ");
        // moov до mdat: файл играет, ещё не дочитанный.
        let (moov_start, _) = find(&out, "moov");
        let (mdat_start, mdat_end) = find(&out, "mdat");
        assert!(moov_start < mdat_start);
        assert_eq!(&out[mdat_start..mdat_end], frames.concat().as_slice());

        let (stsz, _) = find(&out, "moov/trak/mdia/minf/stbl/stsz");
        assert_eq!(be(&out, stsz + 8) as usize, frames.len());
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(be(&out, stsz + 12 + i * 4) as usize, frame.len());
        }
        // stco: каждый кусок указывает на первый кадр своего фрагмента.
        let (stco, _) = find(&out, "moov/trak/mdia/minf/stbl/stco");
        assert_eq!(be(&out, stco + 4), 4);
        let mut offset = mdat_start;
        for chunk in 0..4 {
            assert_eq!(be(&out, stco + 8 + chunk * 4) as usize, offset);
            let count = if chunk == 3 { 5 } else { 10 };
            offset += frames[chunk * 10..chunk * 10 + count].iter().map(Vec::len).sum::<usize>();
        }
        // stsc: 10 кадров в куске, последний — 5.
        let (stsc, _) = find(&out, "moov/trak/mdia/minf/stbl/stsc");
        assert_eq!(be(&out, stsc + 4), 2);
        assert_eq!((be(&out, stsc + 8), be(&out, stsc + 12)), (1, 10));
        assert_eq!((be(&out, stsc + 20), be(&out, stsc + 24)), (4, 5));
        // stts: одна запись на все кадры по 1024.
        let (stts, _) = find(&out, "moov/trak/mdia/minf/stbl/stts");
        assert_eq!((be(&out, stts + 4), be(&out, stts + 8), be(&out, stts + 12)), (1, 35, 1024));
        // Длительность — сумма кадров.
        let (mvhd, _) = find(&out, "moov/mvhd");
        assert_eq!((be(&out, mvhd + 12), be(&out, mvhd + 16)), (44100, 35 * 1024));
        // AudioSpecificConfig переехал без изменений.
        let (esds, end) = find(&out, "moov/trak/mdia/minf/stbl/stsd");
        assert!(out[esds..end].windows(4).any(|w| w == [0x05, 0x02, 0x12, 0x10]));
    }

    #[test]
    fn tags_are_itunes_items() {
        let (file, _) = fragmented(2, 4);
        let cover = vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
        let tags =
            Tags { title: Some(" Название ".into()), artist: None, album: Some("Альбом".into()), cover: Some(cover.clone()) };
        let out = from_fragmented(&file, &tags).expect("m4a");
        let (start, end) = find(&out, "moov/udta/meta/ilst");
        let ilst = &out[start..end];
        let item = |kind: &[u8]| -> Option<(u32, Vec<u8>)> {
            let mut i = 0;
            while i + 8 <= ilst.len() {
                let size = be(ilst, i) as usize;
                if &ilst[i + 4..i + 8] == kind {
                    let data = &ilst[i + 8..i + size];
                    assert_eq!(&data[4..8], b"data");
                    return Some((be(data, 8), data[16..].to_vec()));
                }
                i += size;
            }
            None
        };
        assert_eq!(item(&[0xA9, b'n', b'a', b'm']), Some((1, "Название".as_bytes().to_vec())));
        assert_eq!(item(&[0xA9, b'A', b'R', b'T']), None);
        assert_eq!(item(&[0xA9, b'a', b'l', b'b']), Some((1, "Альбом".as_bytes().to_vec())));
        assert_eq!(item(&[0xA9, b't', b'o', b'o']), Some((1, b"Melogold".to_vec())));
        assert_eq!(item(b"covr"), Some((13, cover)));
    }

    #[test]
    fn incomplete_stream_is_an_error() {
        let (file, _) = fragmented(3, 4);
        assert!(from_fragmented(&file[..file.len() - 10], &Tags::default()).is_err());
    }
}
