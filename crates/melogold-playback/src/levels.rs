//! Уровни звука для столбиков «играет» (docs/PROMPT.md §4, Android `AudioLevels.kt`, Windows
//! `AudioLevels.cs`): низы (до 250 Гц), середина, верх (от 2 кГц). Сэмплы берутся в конвейере до
//! регулятора громкости — столбики показывают музыку, а не положение ползунка, и поправка на
//! громкость, как у Windows, не нужна.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Без новых сэмплов дольше этого (пауза, перемотка) уровни плавно уходят к нулю.
const STALE: Duration = Duration::from_millis(120);

#[derive(Default)]
struct State {
    low: f32,
    mid: f32,
    high: f32,
    updated: Option<Instant>,
    filters: Option<BandFilters>,
}

#[derive(Default)]
pub struct AudioLevels {
    state: Mutex<State>,
}

impl AudioLevels {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Уровни 0…1 со сглаживанием: вверх быстро, вниз плавно; без звука — к нулю.
    pub fn levels(&self) -> (f32, f32, f32) {
        let mut state = self.lock();
        if state.updated.is_none_or(|at| at.elapsed() > STALE) {
            state.low = smooth(state.low, 0.0);
            state.mid = smooth(state.mid, 0.0);
            state.high = smooth(state.high, 0.0);
        }
        (state.low, state.mid, state.high)
    }

    /// Сэмплы F32 вперемешку по каналам: RMS каждой полосы → 0…1 по шкале −50…−8 дБ.
    pub fn feed(&self, samples: &[f32], channels: usize, rate: u32) {
        if channels == 0 || samples.len() < channels || rate == 0 {
            return;
        }
        let mut state = self.lock();
        let filters = state.filters.get_or_insert_with(|| BandFilters::new(rate));
        if filters.rate != rate {
            *filters = BandFilters::new(rate);
        }
        let (mut low, mut mid, mut high) = (0f64, 0f64, 0f64);
        let frames = samples.len() / channels;
        for frame in samples.chunks_exact(channels) {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            let (l, m, h) = filters.next(mono);
            low += f64::from(l * l);
            mid += f64::from(m * m);
            high += f64::from(h * h);
        }
        let rms = |sum: f64| (sum / frames as f64).sqrt();
        state.low = smooth(state.low, level(rms(low)));
        state.mid = smooth(state.mid, level(rms(mid)));
        state.high = smooth(state.high, level(rms(high)));
        state.updated = Some(Instant::now());
    }
}

fn level(rms: f64) -> f32 {
    ((20.0 * rms.max(1e-6).log10() + 50.0) / 42.0).clamp(0.0, 1.0) as f32
}

fn smooth(current: f32, target: f32) -> f32 {
    if target > current {
        current + (target - current) * 0.6
    } else {
        current + (target - current) * 0.15
    }
}

/// Однополюсные фильтры: низы — ниже 250 Гц, верх — выше 2 кГц, середина — между ними.
struct BandFilters {
    rate: u32,
    a250: f32,
    a2000: f32,
    lp250: f32,
    lp2000: f32,
}

impl BandFilters {
    fn new(rate: u32) -> BandFilters {
        let alpha = |cutoff: f64| {
            let rc = 1.0 / (2.0 * std::f64::consts::PI * cutoff);
            let dt = 1.0 / f64::from(rate);
            (dt / (rc + dt)) as f32
        };
        BandFilters { rate, a250: alpha(250.0), a2000: alpha(2000.0), lp250: 0.0, lp2000: 0.0 }
    }

    fn next(&mut self, x: f32) -> (f32, f32, f32) {
        self.lp250 += self.a250 * (x - self.lp250);
        self.lp2000 += self.a2000 * (x - self.lp2000);
        (self.lp250, self.lp2000 - self.lp250, x - self.lp2000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frequency: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .flat_map(|i| {
                let v = (0.5 * (2.0 * std::f64::consts::PI * frequency * i as f64 / f64::from(rate)).sin()) as f32;
                [v, v]
            })
            .collect()
    }

    #[test]
    fn bands_follow_the_tone() {
        let levels = AudioLevels::default();
        for chunk in tone(80.0, 48_000, 0.5).chunks(960) {
            levels.feed(chunk, 2, 48_000);
        }
        let (low, _, high) = levels.levels();
        assert!(low > 0.8 && high < 0.5, "низы {low}, верх {high}");
        let levels = AudioLevels::default();
        for chunk in tone(6000.0, 48_000, 0.5).chunks(960) {
            levels.feed(chunk, 2, 48_000);
        }
        let (low, _, high) = levels.levels();
        assert!(high > 0.8 && low < 0.5, "низы {low}, верх {high}");
    }

    #[test]
    fn silence_decays() {
        let levels = AudioLevels::default();
        levels.feed(&tone(440.0, 48_000, 0.1), 2, 48_000);
        std::thread::sleep(STALE + Duration::from_millis(20));
        let before = levels.levels().1;
        for _ in 0..40 {
            levels.levels();
        }
        assert!(levels.levels().1 < before * 0.1);
    }
}
