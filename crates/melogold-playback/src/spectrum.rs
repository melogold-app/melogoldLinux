//! Спектр для живого фона «Сейчас играет» (задание 0032): БПФ 2048, шаг 512, окно Хэмминга, 48 полос
//! от 30 Гц до 16 кГц по логарифму, наклон +3 дБ на октаву. Числа — общие для всех платформ.
//!
//! Каждое измерение помечено временем звука в треке: окно рисует то, что сейчас звучит (по позиции
//! плеера), а не то, что только что прошло через конвейер, — иначе картинка опережала бы звук на
//! задержку вывода.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::Mutex;
use std::time::Duration;

/// Полос у фона.
pub const BANDS: usize = 48;
const SIZE: usize = 2048;
const HOP: usize = 512;
/// Измерений в запасе — около трёх секунд: позиция плеера отстаёт от конвейера на доли секунды.
const KEEP: usize = 300;

/// Полосы одного окна сэмплов (моно, `SIZE` штук): значения 0..1 по §2 задания 0032.
pub fn bands_of(samples: &[f32], rate: u32) -> [f32; BANDS] {
    let mut re: Vec<f32> = samples.iter().take(SIZE).enumerate().map(|(i, s)| s * hamming(i)).collect();
    re.resize(SIZE, 0.0);
    let mut im = vec![0.0f32; SIZE];
    fft(&mut re, &mut im);
    let norm = (SIZE * SIZE) as f32;
    let power: Vec<f32> = (0..SIZE / 2).map(|i| (re[i] * re[i] + im[i] * im[i]) / norm).collect();
    let bin = rate as f32 / SIZE as f32;
    std::array::from_fn(|band| {
        let lo = 30.0 * (16000.0f32 / 30.0).powf(band as f32 / BANDS as f32);
        let hi = 30.0 * (16000.0f32 / 30.0).powf((band + 1) as f32 / BANDS as f32);
        let from = ((lo / bin) as usize).min(power.len() - 1);
        let to = ((hi / bin) as usize).max(from + 1).min(power.len());
        let mean = power[from..to].iter().sum::<f32>() / (to - from) as f32;
        let tilt = 3.0 * (lo / 30.0).log2();
        ((10.0 * mean.max(1e-12).log10() + tilt + 70.0) / 55.0).clamp(0.0, 1.0)
    })
}

fn hamming(i: usize) -> f32 {
    0.54 - 0.46 * (2.0 * PI * i as f32 / (SIZE - 1) as f32).cos()
}

/// Быстрое преобразование Фурье по основанию 2, на месте.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -2.0 * PI / len as f32;
        let (w_re, w_im) = (angle.cos(), angle.sin());
        for start in (0..n).step_by(len) {
            let (mut c_re, mut c_im) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (t_re, t_im) = (re[b] * c_re - im[b] * c_im, re[b] * c_im + im[b] * c_re);
                re[b] = re[a] - t_re;
                im[b] = im[a] - t_im;
                re[a] += t_re;
                im[a] += t_im;
                (c_re, c_im) = (c_re * w_re - c_im * w_im, c_re * w_im + c_im * w_re);
            }
        }
        len <<= 1;
    }
}

#[derive(Default)]
struct State {
    /// Последние `SIZE` сэмплов моно, по кругу.
    ring: Vec<f32>,
    head: usize,
    filled: usize,
    since_hop: usize,
    frames: VecDeque<(i64, [f32; BANDS])>,
}

#[derive(Default)]
pub struct Spectrum {
    state: Mutex<State>,
}

impl Spectrum {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Сэмплы F32 вперемешку по каналам; `start` — время первого из них в треке (наносекунды).
    pub fn feed(&self, samples: &[f32], channels: usize, rate: u32, start: Option<i64>) {
        if channels == 0 || rate == 0 {
            return;
        }
        let Some(start) = start else { return };
        let mut state = self.lock();
        if state.ring.len() != SIZE {
            state.ring = vec![0.0; SIZE];
        }
        // Перемотка назад или прыжок — прежние измерения не про этот звук.
        if state.frames.back().is_some_and(|(time, _)| start + 500_000_000 < *time || start > *time + 2_000_000_000) {
            state.frames.clear();
            state.filled = 0;
            state.since_hop = 0;
        }
        let per_sample = 1e9 / f64::from(rate);
        for (index, frame) in samples.chunks_exact(channels).enumerate() {
            let mono = frame.iter().sum::<f32>() / channels as f32;
            let head = state.head;
            state.ring[head] = mono;
            state.head = (head + 1) % SIZE;
            state.filled = (state.filled + 1).min(SIZE);
            state.since_hop += 1;
            if state.filled == SIZE && state.since_hop >= HOP {
                state.since_hop = 0;
                let window: Vec<f32> = (0..SIZE).map(|i| state.ring[(state.head + i) % SIZE]).collect();
                // Время окна — его середина.
                let time = start + ((index as f64 - SIZE as f64 / 2.0) * per_sample) as i64;
                let bands = bands_of(&window, rate);
                state.frames.push_back((time, bands));
                while state.frames.len() > KEEP {
                    state.frames.pop_front();
                }
            }
        }
    }

    /// Измерение, которое звучит на `position`: последнее не позже неё, с его временем (по нему окно
    /// отличает новое измерение от уже учтённого).
    pub fn at(&self, position: Duration) -> Option<(i64, [f32; BANDS])> {
        let position = position.as_nanos() as i64;
        let state = self.lock();
        state.frames.iter().rev().find(|(time, _)| *time <= position).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_of(frequency: f32) -> usize {
        (0..BANDS)
            .find(|&b| {
                let hi = 30.0 * (16000.0f32 / 30.0).powf((b + 1) as f32 / BANDS as f32);
                frequency < hi
            })
            .unwrap_or(BANDS - 1)
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        let rate = 48_000;
        let tone: Vec<f32> = (0..SIZE).map(|i| 0.02 * (2.0 * PI * 1000.0 * i as f32 / rate as f32).sin()).collect();
        let bands = bands_of(&tone, rate);
        let loudest = (0..BANDS).max_by(|&a, &b| bands[a].total_cmp(&bands[b])).expect("полосы");
        // Окно растекается на соседний бин, а полосы здесь в несколько бинов: тон делится между своей
        // полосой и соседней.
        assert!((band_of(1000.0)..=band_of(1000.0) + 1).contains(&loudest), "{loudest}: {bands:?}");
        assert!(bands[loudest] > 0.3, "{bands:?}");
    }

    #[test]
    fn silence_is_zero() {
        assert!(bands_of(&[0.0; SIZE], 44_100).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn frames_follow_the_track_time() {
        let spectrum = Spectrum::default();
        let rate = 44_100;
        let second: Vec<f32> = (0..rate as usize)
            .flat_map(|i| {
                let v = 0.02 * (2.0 * PI * 200.0 * i as f32 / rate as f32).sin();
                [v, v]
            })
            .collect();
        spectrum.feed(&second, 2, rate, Some(10_000_000_000));
        assert!(spectrum.at(Duration::from_secs(9)).is_none(), "до начала звука измерений нет");
        let (_, bands) = spectrum.at(Duration::from_millis(10_500)).expect("измерение посреди секунды");
        let loudest = (0..BANDS).max_by(|&a, &b| bands[a].total_cmp(&bands[b])).expect("полосы");
        assert!((band_of(200.0)..=band_of(200.0) + 1).contains(&loudest), "{loudest}: {bands:?}");
        // Перемотка назад: прежние измерения забыты — новое окно копится заново, и до него ничего нет.
        spectrum.feed(&second[..2048 * 2], 2, rate, Some(1_000_000_000));
        assert!(spectrum.at(Duration::from_millis(1_010)).is_none(), "старое не выдаётся за новое");
        assert!(spectrum.at(Duration::from_millis(1_100)).is_some());
    }
}
