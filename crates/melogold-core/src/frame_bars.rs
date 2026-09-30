//! Поля и обводка обложки (Apple `FrameBars.swift`, задание 0014). Обложка всегда квадратная:
//! у кадра видео 16:9 (видео-«статика» с обложкой сингла) и превью 4:3 срезаются поля **любого
//! ровного цвета** (не только чёрные), а внутри них и у квадратных обложек YouTube Music — обводка
//! скана одного цвета со всех четырёх сторон. Пороги строгие: тёмная сцена или небо с одной
//! стороны полем не считаются — поля одного цвета, заметные и одинаковой ширины с двух сторон.

/// Прямоугольник в пикселях.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Линия — поле, если почти все её пиксели в пределах допуска от цвета крайней линии.
const MIN_BAR_SHARE: f64 = 0.98;
/// Допуск линии от цвета крайней линии, по каналу (шум JPEG).
const LINE_TOLERANCE: i32 = 24;
/// Поля с двух сторон — одного цвета, допуск по каналу.
const SIDES_TOLERANCE: i32 = 40;
/// Поля срезаются, только если вместе они не меньше 3 % стороны.
const MIN_BARS: f64 = 0.03;
/// Поля по краям одинаковые: у тёмной сцены тёмен обычно один край.
const MAX_ASYMMETRY: f64 = 0.03;
/// Остаток — не меньше 40 % стороны: почти чёрный кадр остаётся как есть.
const MIN_CONTENT: f64 = 0.4;
/// Обводка — не толще 6 % стороны.
const MAX_BORDER: f64 = 0.06;
/// Вместе с обводкой срезается ещё 0,5 % стороны: размытая граница JPEG.
const BORDER_MARGIN: f64 = 0.005;

/// Пиксели кадра: 4 байта на пиксель (порядок каналов не важен, альфа не участвует), строки через
/// `stride` байт.
pub struct Pixels<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// Где в пикселе цвет: `[0, 1, 2]` у RGBA/BGRA, `[1, 2, 3]` у ARGB.
    pub channels: [usize; 3],
}

type Color = [u8; 3];

impl Pixels<'_> {
    fn at(&self, x: usize, y: usize) -> Color {
        let offset = y * self.stride + x * 4;
        self.channels.map(|c| self.data[offset + c])
    }

    /// Пиксели строки `index` (или столбца) от `from` до `to`.
    fn line(&self, index: usize, from: usize, to: usize, horizontal: bool) -> impl Iterator<Item = Color> + '_ {
        (from..=to).map(move |i| if horizontal { self.at(i, index) } else { self.at(index, i) })
    }

    /// Цвет линии — медиана по каналам: одиночные выбросы его не сдвигают.
    fn color(&self, index: usize, from: usize, to: usize, horizontal: bool) -> Color {
        let mut channels: [Vec<u8>; 3] = Default::default();
        for pixel in self.line(index, from, to, horizontal) {
            for (channel, value) in channels.iter_mut().zip(pixel) {
                channel.push(value);
            }
        }
        channels.map(|mut values| {
            values.sort_unstable();
            values[values.len() / 2]
        })
    }

    /// Почти вся линия — этого цвета (допуск по каналу `LINE_TOLERANCE`).
    fn uniform(&self, index: usize, from: usize, to: usize, horizontal: bool, color: Color) -> bool {
        let count = to - from + 1;
        let allowed = count - (count as f64 * MIN_BAR_SHARE).ceil() as usize;
        let mut off = 0;
        for pixel in self.line(index, from, to, horizontal) {
            if !near(pixel, color, LINE_TOLERANCE) {
                off += 1;
                if off > allowed {
                    return false;
                }
            }
        }
        true
    }
}

fn near(a: Color, b: Color, tolerance: i32) -> bool {
    a.iter().zip(b).all(|(&x, y)| (i32::from(x) - i32::from(y)).abs() <= tolerance)
}

/// Что остаётся без полей и обводки; `None` — срезать нечего. `find_bars` — искать поля (кадр
/// видео); у обложки YouTube Music ищется только обводка.
pub fn content(pixels: &Pixels, find_bars: bool) -> Option<PixelRect> {
    let (width, height) = (pixels.width, pixels.height);
    if width == 0 || height == 0 || pixels.data.len() < (height - 1) * pixels.stride + width * 4 {
        return None;
    }
    let mut rect = PixelRect { x: 0, y: 0, width, height };
    if find_bars {
        // Сначала сверху-снизу, потом по бокам в оставшихся строках.
        let (top, bottom) = bars(pixels, 0, width - 1, height, true);
        let (y0, y1) = accept(top, bottom, height).unwrap_or((0, height - 1));
        let (left, right) = bars(pixels, y0, y1, width, false);
        let (x0, x1) = accept(left, right, width).unwrap_or((0, width - 1));
        rect = PixelRect { x: x0, y: y0, width: x1 - x0 + 1, height: y1 - y0 + 1 };
    }
    if let Some(inner) = border(pixels, rect) {
        rect = inner;
    }
    (rect != PixelRect { x: 0, y: 0, width, height }).then_some(rect)
}

/// Ширина поля с каждой стороны линий `from..=to`; `size` — длина стороны.
/// Поля с двух сторон должны быть одного цвета, иначе — нули.
fn bars(pixels: &Pixels, from: usize, to: usize, size: usize, horizontal: bool) -> (usize, usize) {
    let first_color = pixels.color(0, from, to, horizontal);
    let last_color = pixels.color(size - 1, from, to, horizontal);
    let (mut first, mut last) = (0, size - 1);
    while first < last && pixels.uniform(first, from, to, horizontal, first_color) {
        first += 1;
    }
    while last > first && pixels.uniform(last, from, to, horizontal, last_color) {
        last -= 1;
    }
    if !near(first_color, last_color, SIDES_TOLERANCE) {
        return (0, 0);
    }
    (first, size - 1 - last)
}

/// Оставшиеся линии `first..=last`, если поля подходят.
fn accept(first: usize, last: usize, size: usize) -> Option<(usize, usize)> {
    let size_f = size as f64;
    let fits = (first + last) as f64 >= size_f * MIN_BARS
        && (first as f64 - last as f64).abs() <= size_f * MAX_ASYMMETRY
        && (size - first - last) as f64 >= size_f * MIN_CONTENT;
    fits.then(|| (first, size - 1 - last))
}

/// Обводка скана: рамка одного цвета (по верхней строке) со всех четырёх сторон.
fn border(pixels: &Pixels, rect: PixelRect) -> Option<PixelRect> {
    let (x1, y1) = (rect.x + rect.width - 1, rect.y + rect.height - 1);
    let color = pixels.color(rect.y, rect.x, x1, true);
    // Толщина стороны: подряд идущих линий цвета рамки; больше предела — это не рамка.
    let thickness = |start: usize, from: usize, to: usize, horizontal: bool, forward: bool, size: usize| {
        let cap = ((size as f64 * MAX_BORDER) as usize).max(1);
        let mut count = 0;
        while count <= cap && count < size {
            let index = if forward { start + count } else { start - count };
            if !pixels.uniform(index, from, to, horizontal, color) {
                break;
            }
            count += 1;
        }
        (count >= 1 && count <= cap).then_some(count)
    };
    let top = thickness(rect.y, rect.x, x1, true, true, rect.height)?;
    let bottom = thickness(y1, rect.x, x1, true, false, rect.height)?;
    let left = thickness(rect.x, rect.y, y1, false, true, rect.width)?;
    let right = thickness(x1, rect.y, y1, false, false, rect.width)?;
    let margin = |size: usize| (size as f64 * BORDER_MARGIN).ceil() as usize;
    let (dx, dy) = (margin(rect.width), margin(rect.height));
    let (x0, y0) = (rect.x + left + dx, rect.y + top + dy);
    let (x1, y1) = (x1.checked_sub(right + dx)?, y1.checked_sub(bottom + dy)?);
    (x1 > x0 && y1 > y0).then_some(PixelRect { x: x0, y: y0, width: x1 - x0 + 1, height: y1 - y0 + 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кадр RGBA: `paint` даёт яркость пикселя (x, y).
    fn frame(width: usize, height: usize, paint: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let mut data = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let v = paint(x, y);
                let i = (y * width + x) * 4;
                data[i..i + 3].fill(v);
                data[i + 3] = 255;
            }
        }
        data
    }

    fn content_of(data: &[u8], width: usize, height: usize) -> Option<PixelRect> {
        content(&Pixels { data, width, height, stride: width * 4, channels: [0, 1, 2] }, true)
    }

    fn content_no_bars(data: &[u8], width: usize, height: usize) -> Option<PixelRect> {
        content(&Pixels { data, width, height, stride: width * 4, channels: [0, 1, 2] }, false)
    }

    /// Кадр RGBA цветной: `paint` даёт цвет пикселя.
    fn color_frame(width: usize, height: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut data = vec![255u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let i = (y * width + x) * 4;
                data[i..i + 3].copy_from_slice(&paint(x, y));
            }
        }
        data
    }

    /// Шум JPEG: ±6 по каналу, детерминированный.
    fn noisy(color: [u8; 3], x: usize, y: usize) -> [u8; 3] {
        let n = ((x * 31 + y * 17) % 13) as i32 - 6;
        color.map(|c| (i32::from(c) + n).clamp(0, 255) as u8)
    }

    fn art(x: usize, y: usize) -> [u8; 3] {
        [(40 + (x * 7 + y * 13) % 200) as u8, (60 + (x * 3 + y * 5) % 150) as u8, (30 + (x * 11 + y * 2) % 180) as u8]
    }

    const BROWN: [u8; 3] = [110, 70, 40];
    const BLUE: [u8; 3] = [60, 100, 200];
    const BLACK: [u8; 3] = [8, 8, 8];

    /// «Картинка»: пёстрая, с тёмными местами, но не поле.
    fn picture(x: usize, y: usize) -> u8 {
        (40 + (x * 7 + y * 13) % 200) as u8
    }

    #[test]
    fn square_cover_in_wide_frame_loses_side_bars() {
        let data = frame(1280, 720, |x, y| {
            if (280..1000).contains(&x) {
                picture(x, y)
            } else if x % 3 == 0 {
                12
            } else {
                4
            }
        });
        assert_eq!(content_of(&data, 1280, 720), Some(PixelRect { x: 280, y: 0, width: 720, height: 720 }));
    }

    #[test]
    fn letterboxed_preview_loses_top_and_bottom() {
        let data = frame(480, 360, |x, y| if (45..315).contains(&y) { picture(x, y) } else { 0 });
        assert_eq!(content_of(&data, 480, 360), Some(PixelRect { x: 0, y: 45, width: 480, height: 270 }));
    }

    #[test]
    fn full_frame_stays_as_is() {
        assert_eq!(content_of(&frame(320, 180, picture), 320, 180), None);
    }

    #[test]
    fn dark_scene_on_one_side_is_not_a_bar() {
        let data = frame(320, 180, |x, y| if x < 110 { 6 } else { picture(x, y) });
        assert_eq!(content_of(&data, 320, 180), None);
    }

    #[test]
    fn almost_black_frame_stays_as_is() {
        let data = frame(320, 180, |x, y| if (150..170).contains(&x) { picture(x, y) } else { 0 });
        assert_eq!(content_of(&data, 320, 180), None);
    }

    #[test]
    fn thin_edge_is_not_a_bar() {
        let data = frame(320, 180, |x, y| if !(2..318).contains(&x) { 0 } else { picture(x, y) });
        assert_eq!(content_of(&data, 320, 180), None);
    }

    #[test]
    fn jpeg_noise_in_bars_is_tolerated() {
        let data = frame(320, 180, |x, y| {
            if (70..250).contains(&x) {
                picture(x, y)
            } else if x == 10 && y == 50 {
                200
            } else {
                8
            }
        });
        assert_eq!(content_of(&data, 320, 180), Some(PixelRect { x: 70, y: 0, width: 180, height: 180 }));
    }

    #[test]
    fn brown_bars_with_noise_are_cut() {
        let data = color_frame(1280, 720, |x, y| if (280..1000).contains(&x) { art(x, y) } else { noisy(BROWN, x, y) });
        assert_eq!(content_of(&data, 1280, 720), Some(PixelRect { x: 280, y: 0, width: 720, height: 720 }));
    }

    #[test]
    fn brown_left_blue_right_are_not_bars() {
        let data = color_frame(640, 360, |x, y| {
            if x < 140 {
                noisy(BROWN, x, y)
            } else if x >= 500 {
                noisy(BLUE, x, y)
            } else {
                art(x, y)
            }
        });
        assert_eq!(content_of(&data, 640, 360), None);
    }

    #[test]
    fn flat_sky_on_one_side_is_not_a_bar() {
        let data = color_frame(640, 360, |x, y| if x < 140 { noisy(BLUE, x, y) } else { art(x, y) });
        assert_eq!(content_of(&data, 640, 360), None);
    }

    #[test]
    fn brown_bars_and_black_border_inside_are_cut_together() {
        // Поля 140 px, внутри чёрная обводка 6 px со всех сторон.
        let data = color_frame(640, 360, |x, y| {
            if !(140..500).contains(&x) {
                noisy(BROWN, x, y)
            } else if !(146..494).contains(&x) || !(6..354).contains(&y) {
                noisy(BLACK, x, y)
            } else {
                art(x, y)
            }
        });
        let rect = content_of(&data, 640, 360).expect("срезано");
        // Вся обводка и волосок размытой границы ушли, картинка не задета сверх 0,5 % стороны.
        assert!(rect.x >= 146 && rect.x <= 146 + 3, "{rect:?}");
        assert!(rect.y >= 6 && rect.y <= 6 + 2, "{rect:?}");
        assert!(rect.x + rect.width <= 494 && rect.x + rect.width >= 494 - 3, "{rect:?}");
        assert!(rect.y + rect.height <= 354 && rect.y + rect.height >= 354 - 2, "{rect:?}");
    }

    #[test]
    fn square_cover_with_black_border_loses_it_without_bar_search() {
        let data =
            color_frame(544, 544, |x, y| if !(5..539).contains(&x) || !(5..539).contains(&y) { noisy(BLACK, x, y) } else { art(x, y) });
        let rect = content_no_bars(&data, 544, 544).expect("обводка срезана");
        assert!(rect.x >= 5 && rect.x <= 5 + 3 && rect.y >= 5 && rect.y <= 5 + 3, "{rect:?}");
        assert_eq!(rect.width, rect.height);
    }

    #[test]
    fn wide_flat_background_stays_without_bar_search() {
        // Ровный фон слева и справа по 25 %: без поиска полей остаётся как есть.
        let data = color_frame(640, 360, |x, y| if (160..480).contains(&x) { art(x, y) } else { noisy(BROWN, x, y) });
        assert_eq!(content_no_bars(&data, 640, 360), None);
    }

    #[test]
    fn border_on_three_sides_only_is_not_a_border() {
        let data = color_frame(300, 300, |x, y| if !(5..295).contains(&x) || y >= 295 { BLACK } else { art(x, y) });
        assert_eq!(content_no_bars(&data, 300, 300), None);
    }

    #[test]
    fn too_thick_border_is_not_a_border() {
        // 12 % стороны — уже не обводка, а часть картинки.
        let data = color_frame(300, 300, |x, y| if !(36..264).contains(&x) || !(36..264).contains(&y) { BLACK } else { art(x, y) });
        assert_eq!(content_no_bars(&data, 300, 300), None);
    }
}
