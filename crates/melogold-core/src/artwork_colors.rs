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

/// Палитра страницы: фон, текст, вторичный текст и подложка текущей строки текста песни; кнопка
/// «Играть» и ползунки — цветом обложки (`accent` / `on_accent`), свечение за обложкой — `glow`.
/// Фон ровный, как у Windows: тон сверху, как у Android, дал бы полосу под затуханием края
/// текста — оно нарисовано цветом фона. Фон — свой тон цвета обложки, а не `surface`: у схемы
/// Content поверхность почти нейтральна, и оттенок обложки на ней не читался.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtworkPalette {
    pub background: Rgb,
    pub text: Rgb,
    pub secondary_text: Rgb,
    pub pill: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    pub glow: Rgb,
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
    // Фон — тон цвета обложки с приглушённой насыщенностью: заметно окрашен, но спокоен; подложка
    // строки — на ступень контрастнее фона в обеих темах.
    let background = Hct::from(source.get_hue(), (source.get_chroma() * 0.4).clamp(4.0, 20.0), if dark { 11.0 } else { 94.0 });
    ArtworkPalette {
        background: rgb(background.into()),
        text: rgb(scheme.on_surface()),
        secondary_text: rgb(scheme.on_surface_variant()),
        pill: rgb(scheme.secondary_palette.tone(if dark { 30 } else { 86 })),
        accent: rgb(scheme.primary()),
        on_accent: rgb(scheme.on_primary()),
        glow: rgb(scheme.primary_container()),
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

// ── цвета по долям (задание 0031) ──

/// Сколько цветов фона: преобладающий и до пяти остальных.
const MAX_SHARES: usize = 6;
/// Цвет меньше этой доли на фоне не рисуется: шум сжатия и точка в углу — не цвета обложки. Порог
/// низкий намеренно: радуга «The Dark Side of the Moon» — 1–2 % на цвет, и без неё обложка — просто
/// чёрная.
const MIN_SHARE: f64 = 0.005;
/// Расстояние в Lab, ближе которого цвета сливаются в один: тени и блики одного цвета — один цвет.
const MERGE_DISTANCE: f64 = 16.0;

/// Цвет обложки и его доля в ней (0..1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorShare {
    pub rgb: Rgb,
    pub share: f64,
}

/// Цвета обложки по долям, преобладающий — первым; доли в сумме дают 1.
///
/// В отличие от [`seed`] здесь не выбирается «лучший» цвет: Score отдаёт предпочтение яркому, и у
/// «The Dark Side of the Moon» фон выходил красным из тонкой радуги на чёрном. Здесь чёрный и
/// серый — такие же цвета, как остальные: обложка почти вся чёрная — фон почти весь чёрный, а
/// радуга — маленькими пятнами. Полосы letterbox не срезаются: у квадратной обложки чёрный край —
/// часть рисунка.
pub fn shares(rgba: &[u8], width: usize, height: usize, stride: usize) -> Vec<ColorShare> {
    if width == 0 || height == 0 || stride < width * 4 || rgba.len() < stride * (height - 1) + width * 4 {
        return Vec::new();
    }
    // Точки, а не средние по клеткам: усреднение превращает мелкий цветной узор в серый, и доля
    // серого вырастала из ничего.
    let (columns, rows) = sample_size(width, height);
    let pixels: Vec<Argb> = (0..rows)
        .flat_map(|y| (0..columns).map(move |x| (y, x)))
        .map(|(y, x)| {
            let i = (y * height / rows).min(height - 1) * stride + (x * width / columns).min(width - 1) * 4;
            Argb::new(255, rgba[i], rgba[i + 1], rgba[i + 2])
        })
        .collect();
    let quantized = QuantizerCelebi::quantize(&pixels, MAX_COLORS).color_to_count;
    let mut colors: Vec<(Argb, u64)> = quantized.into_iter().map(|(argb, count)| (argb, u64::from(count))).collect();
    colors.sort_by(|a, b| b.1.cmp(&a.1).then(rgb(a.0).cmp(&rgb(b.0))));

    // Слияние близких: цвет группы — её самый частый цвет, без усреднения. Среднее в Lab уплывало к
    // серому, и радуга «A Head Full of Dreams» превращалась в серую кашу.
    let mut clusters: Vec<([f64; 3], f64)> = Vec::new();
    for (argb, count) in colors {
        let lab = to_lab(rgb(argb));
        match clusters.iter_mut().find(|(center, _)| distance(center, &lab) < MERGE_DISTANCE) {
            Some((_, total)) => *total += count as f64,
            None => clusters.push((lab, count as f64)),
        }
    }
    clusters.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total: f64 = clusters.iter().map(|(_, weight)| weight).sum();
    if total <= 0.0 {
        return Vec::new();
    }
    let mut kept: Vec<ColorShare> = clusters
        .into_iter()
        .map(|(lab, weight)| ColorShare { rgb: from_lab(lab), share: weight / total })
        .filter(|color| color.share >= MIN_SHARE)
        .take(MAX_SHARES)
        .collect();
    let sum: f64 = kept.iter().map(|color| color.share).sum();
    for color in &mut kept {
        color.share /= sum;
    }
    kept
}

/// Цвет фона под светлый текст: оттенок и насыщенность прежние, светлота — в пределах, где белый
/// текст читается (`L*` от 6 до 38). Белая обложка даёт серый фон, чёрная — почти чёрный.
pub fn dark_backdrop(color: Rgb) -> Rgb {
    let [l, a, b] = to_lab(color);
    let target = l.clamp(6.0, 38.0);
    // Темнее — чуть меньше насыщенности (у тёмного цвета та же насыщенность кричит), но не вдвое:
    // иначе радуга превращается в грязь.
    let keep = if l > 0.0 { (target / l).sqrt().clamp(0.7, 1.0) } else { 1.0 };
    from_lab([target, a * keep, b * keep])
}

/// Сторона уменьшенной размытой обложки фона «Сейчас играет» (задание 0031).
pub const PREVIEW_SIDE: usize = 24;

/// Обложка для фона «Сейчас играет» (задание 0031): 24 × 24 после трёх проходов размытия
/// «коробкой» 5 × 5, средняя яркость до размытия и цвет подложки. Растянутая на окно, она и есть
/// «размытая обложка» — без размытия всего окна на каждом кадре.
#[derive(Clone, Debug, PartialEq)]
pub struct BackdropPreview {
    /// RGBA, `PREVIEW_SIDE` × `PREVIEW_SIDE`, строки подряд.
    pub rgba: Vec<u8>,
    /// Средняя яркость 0..1 (Rec. 709): светлую обложку под белым текстом приглушают сильнее.
    pub luma: f64,
    /// Подложка — `dark_backdrop` преобладающего цвета.
    pub base: Rgb,
}

pub fn backdrop_preview(rgba: &[u8], width: usize, height: usize, stride: usize) -> Option<BackdropPreview> {
    if width == 0 || height == 0 || stride < width * 4 || rgba.len() < stride * (height - 1) + width * 4 {
        return None;
    }
    const SIDE: usize = PREVIEW_SIDE;
    let mut cells = vec![[0f64; 3]; SIDE * SIDE];
    let mut luma = 0.0;
    for (index, cell) in cells.iter_mut().enumerate() {
        let (cx, cy) = (index % SIDE, index / SIDE);
        let (x0, y0) = (cx * width / SIDE, cy * height / SIDE);
        let (x1, y1) = (((cx + 1) * width / SIDE).max(x0 + 1).min(width), ((cy + 1) * height / SIDE).max(y0 + 1).min(height));
        let mut n = 0.0;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = y * stride + x * 4;
                for (c, sum) in cell.iter_mut().enumerate() {
                    *sum += f64::from(rgba[i + c]);
                }
                n += 1.0;
            }
        }
        for sum in cell.iter_mut() {
            *sum /= f64::max(n, 1.0);
        }
        luma += (0.2126 * cell[0] + 0.7152 * cell[1] + 0.0722 * cell[2]) / 255.0;
    }
    luma /= (SIDE * SIDE) as f64;
    for _ in 0..3 {
        let copy = cells.clone();
        for y in 0..SIDE {
            for x in 0..SIDE {
                let mut sum = [0f64; 3];
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let sx = (x as i32 + dx).clamp(0, SIDE as i32 - 1) as usize;
                        let sy = (y as i32 + dy).clamp(0, SIDE as i32 - 1) as usize;
                        for (c, total) in sum.iter_mut().enumerate() {
                            *total += copy[sy * SIDE + sx][c];
                        }
                    }
                }
                cells[y * SIDE + x] = sum.map(|v| v / 25.0);
            }
        }
    }
    let base = dark_backdrop(shares(rgba, width, height, stride).first().map_or(0x101010, |c| c.rgb));
    let pixels = cells.iter().flat_map(|p| [p[0].round() as u8, p[1].round() as u8, p[2].round() as u8, 255]).collect();
    Some(BackdropPreview { rgba: pixels, luma, base })
}

fn to_lab(color: Rgb) -> [f64; 3] {
    let channel = |shift: u32| {
        let c = f64::from((color >> shift) & 0xFF) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (channel(16), channel(8), channel(0));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| if t > 216.0 / 24389.0 { t.cbrt() } else { (24389.0 / 27.0 * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn from_lab([l, a, b]: [f64; 3]) -> Rgb {
    let fy = (l + 16.0) / 116.0;
    let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
    let inverse = |t: f64| if t.powi(3) > 216.0 / 24389.0 { t.powi(3) } else { (116.0 * t - 16.0) / (24389.0 / 27.0) };
    let (x, y, z) = (inverse(fx) * 0.95047, inverse(fy), inverse(fz) * 1.08883);
    let linear = [3.2406 * x - 1.5372 * y - 0.4986 * z, -0.9689 * x + 1.8758 * y + 0.0415 * z, 0.0557 * x - 0.2040 * y + 1.0570 * z];
    let gamma = |c: f64| {
        let c = c.clamp(0.0, 1.0);
        let v = if c <= 0.0031308 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
        (v * 255.0).round() as u32
    };
    (gamma(linear[0]) << 16) | (gamma(linear[1]) << 8) | gamma(linear[2])
}

fn distance(a: &[f64; 3], b: &[f64; 3]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt()
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
        assert!(luminance(light.background) > 88.0);
        // Кнопка «Играть» контрастна со своим значком.
        assert!((luminance(light.accent) - luminance(light.on_accent)).abs() > 40.0);
        assert!((luminance(dark.accent) - luminance(dark.on_accent)).abs() > 40.0);
        assert!(luminance(light.text) < 20.0);
        // Фон — с оттенком обложки, а не серый.
        assert!(Hct::new(Argb::from_u32(0xFF00_0000 | dark.background)).get_chroma() > 1.0);
    }

    /// Чёрная обложка с тонкой радугой (как у «The Dark Side of the Moon»): преобладает чёрный, а не
    /// красный, как выбирал Score.
    #[test]
    fn a_black_cover_with_a_thin_rainbow_is_mostly_black() {
        let (width, height) = (200usize, 200usize);
        let mut rgba = vec![0u8; width * height * 4];
        let rainbow = [[230, 30, 30], [240, 200, 20], [30, 160, 60], [40, 90, 220]];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                let color = if (90..110).contains(&y) && x > 100 { rainbow[(y - 90) / 5] } else { [2, 2, 2] };
                rgba[i..i + 3].copy_from_slice(&color);
                rgba[i + 3] = 255;
            }
        }
        let shares = shares(&rgba, width, height, width * 4);
        let first = shares[0];
        assert!(to_lab(first.rgb)[0] < 10.0, "первым — чёрный: {shares:?}");
        assert!(first.share > 0.85, "{shares:?}");
        assert!(shares.len() >= 3, "цвета радуги тоже есть, маленькими долями: {shares:?}");
        assert!((shares.iter().map(|c| c.share).sum::<f64>() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn backdrop_keeps_white_text_readable() {
        assert!(to_lab(dark_backdrop(0xFFFFFF))[0] <= 38.5, "белая обложка — серый фон");
        assert!(to_lab(dark_backdrop(0x020202))[0] >= 5.5, "чёрная — почти чёрный, не провал");
        let red = to_lab(dark_backdrop(0xE01E1E));
        assert!(red[1] > 20.0, "красный остаётся красным: {red:?}");
    }

    #[test]
    fn lab_round_trip() {
        for color in [0x000000, 0xFFFFFF, 0xE01E1E, 0x2D5AE0, 0x7F7F7F, 0x12AB34] {
            let back = from_lab(to_lab(color));
            for shift in [16, 8, 0] {
                let (a, b) = ((color >> shift) & 0xFF, (back >> shift) & 0xFF);
                assert!(a.abs_diff(b) <= 1, "{color:06x} → {back:06x}");
            }
        }
    }

    #[test]
    fn a_flat_cover_gives_a_flat_preview() {
        let (width, height) = (100usize, 60usize);
        let rgba: Vec<u8> = (0..width * height).flat_map(|_| [200u8, 40, 40, 255]).collect();
        let preview = backdrop_preview(&rgba, width, height, width * 4).expect("превью");
        assert_eq!(preview.rgba.len(), PREVIEW_SIDE * PREVIEW_SIDE * 4);
        assert!(preview.rgba.chunks(4).all(|p| p == [200, 40, 40, 255]), "однотонная остаётся однотонной");
        let expected = (0.2126 * 200.0 + 0.7152 * 40.0 + 0.0722 * 40.0) / 255.0;
        assert!((preview.luma - expected).abs() < 1e-9, "{}", preview.luma);
        assert_eq!(preview.base, dark_backdrop(0xC82828));
    }
}
