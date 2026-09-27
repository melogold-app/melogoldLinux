//! Цвета «Сейчас играет» из обложки трека — как у Android (`ui/theme/ArtworkColorScheme.kt`) и
//! Windows (`Services/ArtworkColors.cs`): уменьшенная копия без чёрных полос, QuantizerCelebi,
//! Score по цветным пикселям, схема Content вокруг победившего цвета. У серой обложки (цветных
//! пикселей меньше 5 %) цвета нет — «Сейчас играет» остаётся в цветах темы.

use material_colors::color::Argb;
use material_colors::hct::Hct;
use material_colors::quantize::{Quantizer, QuantizerCelebi};
use material_colors::scheme::variant::SchemeContent;
use material_colors::score::Score;

/// Ширина уменьшенной копии: цвета те же, а квантование — миллисекунды.
const SAMPLE_SIZE: usize = 112;
const MAX_COLORS: usize = 128;
/// Ниже — серый без оттенка: он перевесил бы долю оттенков у Score.
const MIN_CHROMA: f64 = 8.0;
const MIN_COLORFUL_SHARE: f64 = 0.05;
/// Полоса letterbox — строка или столбец, где 90 % пикселей темнее 24 по каждому каналу.
const BAR_MAX_CHANNEL: u8 = 24;
const BAR_MIN_SHARE: f64 = 0.9;
const MIN_CONTENT: usize = 3;

/// Цвет в `0xRRGGBB`.
pub type Rgb = u32;

/// Палитра страницы: фон, текст, вторичный текст и подложка текущей строки текста песни.
/// Фон ровный, как у Windows: тон сверху, как у Android, дал бы полосу под затуханием края
/// текста — оно нарисовано цветом фона.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtworkPalette {
    pub background: Rgb,
    pub text: Rgb,
    pub secondary_text: Rgb,
    pub pill: Rgb,
}

/// Цвет-зерно обложки по её пикселям RGBA (`stride` — байт в строке); `None` — обложка серая.
pub fn seed(rgba: &[u8], width: usize, height: usize, stride: usize) -> Option<Rgb> {
    if width == 0 || height == 0 || stride < width * 4 || rgba.len() < stride * (height - 1) + width * 4 {
        return None;
    }
    let pixels = sample(rgba, width, height, stride);
    let (sample_width, sample_height) = sample_size(width, height);
    let content = without_letterbox(&pixels, sample_width, sample_height);
    let mut colors = QuantizerCelebi::quantize(&content, MAX_COLORS).color_to_count;
    let total: u64 = colors.values().map(|&count| u64::from(count)).sum();
    colors.retain(|argb, _| Hct::new(*argb).get_chroma() >= MIN_CHROMA);
    let colorful: u64 = colors.values().map(|&count| u64::from(count)).sum();
    if total == 0 || (colorful as f64) < total as f64 * MIN_COLORFUL_SHARE {
        return None;
    }
    let best = Score::score(&colors, None, None, None).into_iter().next()?;
    Some(rgb(best))
}

/// Палитра вокруг зерна для светлой или тёмной темы.
pub fn palette(seed: Rgb, dark: bool) -> ArtworkPalette {
    let source = Hct::new(Argb::from_u32(0xFF00_0000 | seed));
    let scheme = SchemeContent::new(source, dark, None).scheme;
    ArtworkPalette {
        background: rgb(scheme.surface()),
        text: rgb(scheme.on_surface()),
        secondary_text: rgb(scheme.on_surface_variant()),
        pill: rgb(scheme.secondary_container()),
    }
}

fn rgb(argb: Argb) -> Rgb {
    (u32::from(argb.red) << 16) | (u32::from(argb.green) << 8) | u32::from(argb.blue)
}

fn sample_size(width: usize, height: usize) -> (usize, usize) {
    let sample_height = ((SAMPLE_SIZE * height) as f64 / width as f64).round() as usize;
    (SAMPLE_SIZE, sample_height.clamp(1, SAMPLE_SIZE * 2))
}

/// Уменьшенная копия усреднением по клеткам (как `Fant` у Windows и `createScaledBitmap` с
/// фильтром у Android): мелкие детали не перевешивают крупные пятна цвета.
fn sample(rgba: &[u8], width: usize, height: usize, stride: usize) -> Vec<Argb> {
    let (out_width, out_height) = sample_size(width, height);
    let mut out = Vec::with_capacity(out_width * out_height);
    for y in 0..out_height {
        let (y0, y1) = (y * height / out_height, ((y + 1) * height / out_height).max(y * height / out_height + 1).min(height));
        for x in 0..out_width {
            let (x0, x1) = (x * width / out_width, ((x + 1) * width / out_width).max(x * width / out_width + 1).min(width));
            let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
            for row in y0..y1 {
                for column in x0..x1 {
                    let i = row * stride + column * 4;
                    r += u64::from(rgba[i]);
                    g += u64::from(rgba[i + 1]);
                    b += u64::from(rgba[i + 2]);
                    n += 1;
                }
            }
            let n = n.max(1);
            out.push(Argb::new(255, (r / n) as u8, (g / n) as u8, (b / n) as u8));
        }
    }
    out
}

fn is_bar(pixel: &Argb) -> bool {
    pixel.red.max(pixel.green).max(pixel.blue) <= BAR_MAX_CHANNEL
}

/// Пиксели без чёрных полос сверху, снизу и по бокам; почти чёрная обложка остаётся как есть.
fn without_letterbox(pixels: &[Argb], width: usize, height: usize) -> Vec<Argb> {
    let row_is_bar = |y: usize| (0..width).filter(|&x| is_bar(&pixels[y * width + x])).count() as f64 >= width as f64 * BAR_MIN_SHARE;
    let (mut top, mut bottom) = (0, height - 1);
    while top < bottom && row_is_bar(top) {
        top += 1;
    }
    while bottom > top && row_is_bar(bottom) {
        bottom -= 1;
    }
    let rows = bottom - top + 1;
    let column_is_bar = |x: usize| (top..=bottom).filter(|&y| is_bar(&pixels[y * width + x])).count() as f64 >= rows as f64 * BAR_MIN_SHARE;
    let (mut left, mut right) = (0, width - 1);
    while left < right && column_is_bar(left) {
        left += 1;
    }
    while right > left && column_is_bar(right) {
        right -= 1;
    }
    let columns = right - left + 1;
    if rows < MIN_CONTENT || columns < MIN_CONTENT {
        return pixels.to_vec();
    }
    (top..=bottom).flat_map(|y| (left..=right).map(move |x| pixels[y * width + x])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize, pixel: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = pixel(x, y);
                out.extend_from_slice(&[r, g, b, 255]);
            }
        }
        out
    }

    fn hue_of(rgb: Rgb) -> f64 {
        Hct::new(Argb::from_u32(0xFF00_0000 | rgb)).get_hue()
    }

    #[test]
    fn grey_artwork_has_no_colors() {
        let pixels = image(200, 200, |x, y| {
            let v = ((x + y) % 256) as u8;
            [v, v, v]
        });
        assert_eq!(seed(&pixels, 200, 200, 800), None);
    }

    #[test]
    fn colorful_artwork_gives_its_color() {
        // Бирюзовое море под белым небом, как на обложке «Ultra Chilled».
        let pixels = image(300, 300, |_, y| if y < 120 { [240, 240, 235] } else { [20, 170, 150] });
        let found = seed(&pixels, 300, 300, 1200).expect("цветная обложка");
        let hue = hue_of(found);
        assert!((150.0..=200.0).contains(&hue), "оттенок бирюзы, а не {hue}");
    }

    #[test]
    fn letterbox_does_not_decide() {
        // Кадр видео 16:9 в квадрате: сверху и снизу чёрные полосы, посередине красное.
        let pixels = image(320, 320, |_, y| if (70..250).contains(&y) { [200, 40, 40] } else { [0, 0, 0] });
        let found = seed(&pixels, 320, 320, 1280).expect("красный кадр");
        let hue = hue_of(found);
        assert!(hue < 40.0 || hue > 340.0, "красный, а не {hue}");
    }

    #[test]
    fn a_few_colored_pixels_are_not_enough() {
        // Чёрно-белая обложка с крошечной цветной подписью: меньше 5 % цветных.
        let pixels = image(200, 200, |x, y| {
            if x < 8 && y < 8 {
                [220, 30, 30]
            } else if (x / 20 + y / 20) % 2 == 0 {
                [0, 0, 0]
            } else {
                [255, 255, 255]
            }
        });
        assert_eq!(seed(&pixels, 200, 200, 800), None);
    }

    #[test]
    fn palette_is_dark_on_dark_and_light_on_light() {
        let luminance = |rgb: Rgb| Hct::new(Argb::from_u32(0xFF00_0000 | rgb)).get_tone();
        let dark = palette(0x14AA96, true);
        let light = palette(0x14AA96, false);
        assert!(luminance(dark.background) < 20.0);
        assert!(luminance(dark.text) > 80.0);
        assert!(luminance(light.background) > 90.0);
        assert!(luminance(light.text) < 20.0);
        // Фон — с оттенком обложки, а не серый.
        assert!(Hct::new(Argb::from_u32(0xFF00_0000 | dark.background)).get_chroma() > 1.0);
    }
}
