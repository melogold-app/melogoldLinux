//! Звук трека по фрагментам (Windows `AacStreamSource.cs`): начало файла — одним запросом на 64 КБ
//! (`moov`, `sidx` и первые секунды первого фрагмента), остальное — фрагмент за фрагментом
//! (~10 с звука каждый), два следующих — заранее, пока звук уже идёт.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::fmp4::{self, Index, Sample};
use crate::reader::RangeReader;
use crate::resolver::{StreamError, StreamErrorKind, StreamInfo};

const HEAD_BYTES: usize = 64 * 1024;
const MAX_HEAD: usize = 4 * 1024 * 1024;

/// Фрагмент: все его байты (или только начало) и кадры.
pub struct FragmentData {
    pub bytes: Vec<u8>,
    pub available: usize,
    pub samples: Vec<Sample>,
}

/// Фрагмент, который грузится один раз, сколько бы его ни просили.
type FragmentCell = Arc<tokio::sync::OnceCell<Result<Arc<FragmentData>, StreamError>>>;

pub struct TrackStream {
    reader: Arc<RangeReader>,
    index: Index,
    head: Vec<u8>,
    runtime: tokio::runtime::Handle,
    fragments: Mutex<HashMap<usize, FragmentCell>>,
}

impl TrackStream {
    /// Начало файла и таблица фрагментов. Длинный индекс (часовые видео) дочитывается.
    pub async fn open(reader: Arc<RangeReader>) -> Result<Arc<TrackStream>, StreamError> {
        let mut head = reader.read(0, HEAD_BYTES).await?;
        let index = loop {
            match fmp4::parse_index(&head) {
                Ok(index) => break index,
                Err(error) if error.0 == "truncated" && head.len() < MAX_HEAD => {
                    let more = reader.read(head.len() as u64, head.len().max(256 * 1024)).await?;
                    if more.is_empty() {
                        return Err(StreamError::new(StreamErrorKind::Extractor, "Unsupported stream: truncated index"));
                    }
                    head.extend_from_slice(&more);
                }
                Err(error) => return Err(StreamError::new(StreamErrorKind::Extractor, format!("Unsupported stream: {error}"))),
            }
        };
        Ok(Arc::new(TrackStream { reader, index, head, runtime: tokio::runtime::Handle::current(), fragments: Mutex::default() }))
    }

    pub fn index(&self) -> &Index {
        &self.index
    }

    pub fn info(&self) -> StreamInfo {
        self.reader.info()
    }

    pub fn duration(&self) -> Duration {
        self.index.duration()
    }

    pub fn reader(&self) -> &Arc<RangeReader> {
        &self.reader
    }

    pub fn runtime(&self) -> &tokio::runtime::Handle {
        &self.runtime
    }

    fn cell(&self, number: usize) -> FragmentCell {
        let mut fragments = self.fragments.lock().unwrap_or_else(|p| p.into_inner());
        // Держим только соседей: память — на три-четыре фрагмента, не на весь трек.
        fragments.retain(|k, cell| (*k + 2 >= number && *k <= number + 3) && !matches!(cell.get(), Some(Err(_))));
        Arc::clone(fragments.entry(number).or_default())
    }

    /// Начало фрагмента, если оно уже есть в прочитанном заголовке: кадры можно отдавать сразу.
    pub fn from_head(&self, number: usize) -> Option<FragmentData> {
        let fragment = self.index.fragments.get(number)?;
        let offset = usize::try_from(fragment.offset).ok()?;
        let size = fragment.size as usize;
        let in_head = self.head.len().checked_sub(offset)?.min(size);
        if in_head == 0 {
            return None;
        }
        let mut bytes = vec![0u8; size];
        bytes[..in_head].copy_from_slice(&self.head[offset..offset + in_head]);
        let samples = fmp4::parse_fragment(&bytes, in_head, &self.index.track).ok()?;
        (!samples.is_empty()).then_some(FragmentData { bytes, available: in_head, samples })
    }

    /// Фрагмент целиком; один и тот же фрагмент грузится один раз, сколько бы его ни просили.
    pub async fn fragment(self: &Arc<Self>, number: usize) -> Result<Arc<FragmentData>, StreamError> {
        let cell = self.cell(number);
        let this = Arc::clone(self);
        cell.get_or_init(|| async move { this.load(number).await.map(Arc::new) }).await.clone()
    }

    async fn load(&self, number: usize) -> Result<FragmentData, StreamError> {
        let fragment = *self.index.fragments.get(number).ok_or_else(|| StreamError::new(StreamErrorKind::Extractor, "no such fragment"))?;
        let size = fragment.size as usize;
        let mut bytes = vec![0u8; size];
        // Начало фрагмента может уже лежать в заголовке — дочитывается только остаток.
        let offset = fragment.offset as usize;
        let in_head = self.head.len().saturating_sub(offset).min(size);
        bytes[..in_head].copy_from_slice(&self.head[offset.min(self.head.len())..offset.min(self.head.len()) + in_head]);
        let mut have = in_head;
        while have < size {
            let chunk = self.reader.read(fragment.offset + have as u64, size - have).await?;
            if chunk.is_empty() {
                break;
            }
            bytes[have..have + chunk.len()].copy_from_slice(&chunk);
            have += chunk.len();
        }
        let samples = fmp4::parse_fragment(&bytes, have, &self.index.track)
            .map_err(|e| StreamError::new(StreamErrorKind::Extractor, format!("Unsupported stream: {e}")))?;
        Ok(FragmentData { bytes, available: have, samples })
    }

    /// Загрузить заранее: ошибку увидит чтение этого фрагмента и загрузит его заново.
    pub fn prefetch(self: &Arc<Self>, number: usize) {
        if number >= self.index.fragments.len() || self.reader.is_cancelled() {
            return;
        }
        let this = Arc::clone(self);
        self.runtime.spawn(async move {
            let _ = this.fragment(number).await;
        });
    }

    /// Блокирующее чтение фрагмента — из потока, который кормит GStreamer.
    pub fn fragment_blocking(self: &Arc<Self>, number: usize) -> Result<Arc<FragmentData>, StreamError> {
        let this = Arc::clone(self);
        self.runtime.block_on(async move { this.fragment(number).await })
    }

    /// Прочитать весь трек в кэш (загрузка, «Сохранить файлом»).
    pub async fn read_all(self: &Arc<Self>) -> Result<(), StreamError> {
        for number in 0..self.index.fragments.len() {
            self.fragment(number).await?;
        }
        Ok(())
    }
}
