//! 图像 → 字符画：降采样 + 字符量化。纯计算，不做任何终端 I/O。
//!
//! 原理沿用原项目（yaochao/img_to_txt）：把每个字符格覆盖的像素块取平均，
//! 再把「平均亮度」量化成视觉密度相近的字符。
//!
//! 相对原版的升级：半块字符 `▀` 让一个字符格承载**两个**像素（上=前景色、
//! 下=背景色），纵向分辨率翻倍且不丢颜色；盲文点阵 `⠿` 每格 2x4 = 8 个采样点。

/// 原版的 62 级亮度阶梯（密度从重到轻）。
pub const LADDER: &str = "##@@MMBB88NNHHOOGGPPEEXXFFVVYY22ZZCC77LLjjll11rrii;;;:::....  ";

/// 4x4 有序抖动矩阵，让盲文模式的灰度过渡不至于只剩黑白。
const BAYER4: [[u8; 4]; 4] = [
    [0, 8, 2, 10],
    [12, 4, 14, 6],
    [3, 11, 1, 9],
    [15, 7, 13, 5],
];

/// 2x4 盲文点阵的位映射（(dy, dx) → U+2800 的 bit）。
const BRAILLE_BITS: [[u16; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// 半块 `▀`：2 子像素/格，全彩
    Half,
    /// 单字符：1 子像素/格，单色
    Ascii,
    /// 盲文点阵：8 子像素/格，双色
    Braille,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "half" | "block" => Some(Mode::Half),
            "ascii" | "text" => Some(Mode::Ascii),
            "braille" | "dots" => Some(Mode::Braille),
            _ => None,
        }
    }

    /// 每个字符格覆盖的子像素数 (x, y)。
    pub fn subpixels(self) -> (usize, usize) {
        match self {
            Mode::Half => (1, 2),
            Mode::Ascii => (1, 1),
            Mode::Braille => (2, 4),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Half => "half",
            Mode::Ascii => "ascii",
            Mode::Braille => "braille",
        }
    }
}

/// 已渲染好的字符画：字符 + 每格前景/背景色。
#[derive(Clone)]
pub struct Canvas {
    pub cols: usize,
    pub rows: usize,
    pub ch: Vec<char>,
    pub fg: Vec<[u8; 3]>,
    pub bg: Option<Vec<[u8; 3]>>,
}

/// 由列数推行数：终端字符宽高比约 1:2，所以纵向要除以 2 才不变形。
pub fn rows_for(cols: usize, w: usize, h: usize) -> usize {
    let v = cols as f64 * h as f64 / w as f64 * 0.5;
    (v.round() as usize).max(1)
}

pub fn render(rgb: &[u8], w: usize, h: usize, cols: usize, mode: Mode) -> Canvas {
    let cols = cols.max(1);
    let rows = rows_for(cols, w, h);
    let (sx, sy) = mode.subpixels();
    let gw = cols * sx;
    let gh = rows * sy;
    let samples = downsample(rgb, w, h, gw, gh);

    match mode {
        Mode::Half => build_half(&samples, cols, rows, gw),
        Mode::Ascii => build_ascii(&samples, cols, rows),
        Mode::Braille => build_braille(&samples, cols, rows, gw),
    }
}

/// 面积平均降采样：每个目标像素取源图对应矩形块的算术平均。
///
/// 用 f64 累加而不是 u8 —— 原版 Python 实现就是在这里踩过 uint8 回绕的坑。
fn downsample(rgb: &[u8], w: usize, h: usize, ow: usize, oh: usize) -> Vec<[f32; 3]> {
    let mut out = vec![[0f32; 3]; ow * oh];
    let xs: Vec<usize> = (0..=ow).map(|i| i * w / ow).collect();
    let ys: Vec<usize> = (0..=oh).map(|i| i * h / oh).collect();

    for oy in 0..oh {
        let y0 = ys[oy];
        let y1 = ys[oy + 1].max(y0 + 1).min(h);
        for ox in 0..ow {
            let x0 = xs[ox];
            let x1 = xs[ox + 1].max(x0 + 1).min(w);
            let (mut r, mut g, mut b) = (0f64, 0f64, 0f64);
            let mut n = 0f64;
            for y in y0..y1 {
                let mut i = (y * w + x0) * 3;
                for _ in x0..x1 {
                    r += rgb[i] as f64;
                    g += rgb[i + 1] as f64;
                    b += rgb[i + 2] as f64;
                    i += 3;
                    n += 1.0;
                }
            }
            let inv = if n > 0.0 { 1.0 / n } else { 0.0 };
            out[oy * ow + ox] = [(r * inv) as f32, (g * inv) as f32, (b * inv) as f32];
        }
    }
    out
}

fn to8(c: [f32; 3]) -> [u8; 3] {
    [
        c[0].round().clamp(0.0, 255.0) as u8,
        c[1].round().clamp(0.0, 255.0) as u8,
        c[2].round().clamp(0.0, 255.0) as u8,
    ]
}

fn luminance(c: [f32; 3]) -> f32 {
    c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
}

fn build_half(s: &[[f32; 3]], cols: usize, rows: usize, gw: usize) -> Canvas {
    let ch = vec!['\u{2580}'; cols * rows]; // ▀ 上半块
    let mut fg = vec![[0u8; 3]; cols * rows];
    let mut bg = vec![[0u8; 3]; cols * rows];
    for r in 0..rows {
        for c in 0..cols {
            let i = r * cols + c;
            fg[i] = to8(s[(2 * r) * gw + c]);
            bg[i] = to8(s[(2 * r + 1) * gw + c]);
        }
    }
    Canvas { cols, rows, ch, fg, bg: Some(bg) }
}

fn build_ascii(s: &[[f32; 3]], cols: usize, rows: usize) -> Canvas {
    let ladder: Vec<char> = LADDER.chars().collect();
    let n = ladder.len();
    let step = 256.0 / n as f32;
    let mut ch = vec![' '; cols * rows];
    let mut fg = vec![[0u8; 3]; cols * rows];
    for i in 0..cols * rows {
        let lum = luminance(s[i]);
        let idx = (lum / step) as usize;
        ch[i] = ladder[idx.min(n - 1)];
        fg[i] = to8(s[i]);
    }
    Canvas { cols, rows, ch, fg, bg: None }
}

fn build_braille(s: &[[f32; 3]], cols: usize, rows: usize, gw: usize) -> Canvas {
    let mut ch = vec![' '; cols * rows];
    let mut fg = vec![[0u8; 3]; cols * rows];
    let mut bg = vec![[0u8; 3]; cols * rows];

    for r in 0..rows {
        for c in 0..cols {
            let mut code = 0u16;
            let (mut on_c, mut on_n) = ([0f32; 3], 0f32);
            let (mut off_c, mut off_n) = ([0f32; 3], 0f32);

            for dy in 0..4 {
                for dx in 0..2 {
                    let y = 4 * r + dy;
                    let x = 2 * c + dx;
                    let px = s[y * gw + x];
                    let thr = 0.5 + (BAYER4[y % 4][x % 4] as f32 / 16.0 - 0.5) * 0.9;
                    if luminance(px) / 255.0 > thr {
                        code |= BRAILLE_BITS[dy][dx];
                        for k in 0..3 {
                            on_c[k] += px[k];
                        }
                        on_n += 1.0;
                    } else {
                        for k in 0..3 {
                            off_c[k] += px[k];
                        }
                        off_n += 1.0;
                    }
                }
            }

            let i = r * cols + c;
            ch[i] = char::from_u32(0x2800 + code as u32).unwrap_or(' ');
            let cell = s[(4 * r) * gw + 2 * c];
            fg[i] = if on_n > 0.0 {
                to8([
                    on_c[0] / on_n,
                    on_c[1] / on_n,
                    on_c[2] / on_n,
                ])
            } else {
                to8(cell)
            };
            bg[i] = if off_n > 0.0 {
                to8([
                    off_c[0] / off_n,
                    off_c[1] / off_n,
                    off_c[2] / off_n,
                ])
            } else {
                to8(cell)
            };
        }
    }
    Canvas { cols, rows, ch, fg, bg: Some(bg) }
}
