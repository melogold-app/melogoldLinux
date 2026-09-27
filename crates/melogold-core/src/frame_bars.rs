//! Чёрные поля у кадра видео YouTube (Windows `FrameBars.cs`, задание Windows 0007): квадратная
//! обложка в кадре 16:9 (видео-«статика» с обложкой сингла) — полосы по бокам, превью 4:3 — сверху
//! и снизу. Поля срезаются, чтобы обложка была без них. Порог строгий: тёмная сцена самого видео
//! полем не считается — поля почти целиком чёрные, заметные и одинаковые с двух сторон.

/// Прямоугольник в пикселях.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

/// Чёрный у JPEG — 0…20 по каналу.
const MAX_CHANNEL: u8 = 28;
/// Линия — поле, если почти все её пиксели чёрные.
const MIN_BAR_SHARE: f64 = 0.98;
/// Поля срезаются, только если вместе они не меньше 3 % стороны.
const MIN_BARS: f64 = 0.03;
/// Поля по краям одинаковые: у тёмной сцены тёмен обычно один край.
const MAX_ASYMMETRY: f64 = 0.03;
/// Остаток — не меньше 40 % стороны: почти чёрный кадр остаётся как есть.
const MIN_CONTENT: f64 = 0.4;

/// Пиксели кадра: 4 байта на пиксель (порядок каналов не важен, альфа — последний или первый
/// байт не участвует, если `alpha_first` задан верно), строки через `stride` байт.
pub struct Pixels<'a> {
    pub data: &'a [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// Где в пикселе цвет: `[0, 1, 2]` у RGBA/BGRA, `[1, 2, 3]` у ARGB.
    pub channels: [usize; 3],
}

impl Pixels<'_> {
    fn bright(&self, x: usize, y: usize) -> bool {
        let offset = y * self.stride + x * 4;
        self.channels.iter().any(|&c| self.data[offset + c] > MAX_CHANNEL)
    }
}

/// Что остаётся без полей; `None` — полей нет.
pub fn content(pixels: &Pixels) -> Option<PixelRect> {
    let (width, height) = (pixels.width, pixels.height);
    if width == 0 || height == 0 || pixels.data.len() < (height - 1) * pixels.stride + width * 4 {
        return None;
    }
    let (mut top, mut bottom) = (0, height - 1);
    while top < bottom && line(pixels, top, 0, width - 1, true) {
        top += 1;
    }
    while bottom > top && line(pixels, bottom, 0, width - 1, true) {
        bottom -= 1;
    }
    let (y0, y1) = if accept(top, height - 1 - bottom, height) { (top, bottom) } else { (0, height - 1) };

    let (mut left, mut right) = (0, width - 1);
    while left < right && line(pixels, left, y0, y1, false) {
        left += 1;
    }
    while right > left && line(pixels, right, y0, y1, false) {
        right -= 1;
    }
    let (x0, x1) = if accept(left, width - 1 - right, width) { (left, right) } else { (0, width - 1) };

    if x0 == 0 && y0 == 0 && x1 == width - 1 && y1 == height - 1 {
        return None;
    }
    Some(PixelRect { x: x0, y: y0, width: x1 - x0 + 1, height: y1 - y0 + 1 })
}

fn accept(first: usize, last: usize, size: usize) -> bool {
    let size_f = size as f64;
    (first + last) as f64 >= size_f * MIN_BARS
        && (first as f64 - last as f64).abs() <= size_f * MAX_ASYMMETRY
        && (size - first - last) as f64 >= size_f * MIN_CONTENT
}

/// Строка `index` (или столбец) от `from` до `to` — поле.
fn line(pixels: &Pixels, index: usize, from: usize, to: usize, horizontal: bool) -> bool {
    let count = to - from + 1;
    let allowed = count - (count as f64 * MIN_BAR_SHARE).ceil() as usize;
    let mut bright = 0;
    for i in from..=to {
        let (x, y) = if horizontal { (i, index) } else { (index, i) };
        if pixels.bright(x, y) {
            bright += 1;
            if bright > allowed {
                return false;
            }
        }
    }
    true
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
        content(&Pixels { data, width, height, stride: width * 4, channels: [0, 1, 2] })
    }

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
}
