//! 终端绘制：备用屏 + 绝对定位 + 增量刷新 + 同步输出。
//!
//! 这套机制是解决「频闪」的关键。相对光标回退（`ESC[{n}F`）只要有一帧比可视区高、
//! 或终端多折了一行，位移就会累积漂移 —— 画面被切成几段、下方大片空白。
//! 绝对定位每帧都落在确定坐标，物理上不会漂。

use std::io::{self, Write};

use crate::render::Canvas;

const ESC: &str = "\x1b[";

/// 终端能显示的颜色深度。
///
/// macOS 的 Terminal.app **不支持 24 位真彩色**（只到 256 色）。若强行发
/// `ESC[38;2;r;g;bm`，它会把 `2;r;g;b` 当成一串独立的 SGR 参数，结果就是
/// 颜色全乱 —— 表现为画面被随机彩虹色覆盖。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorDepth {
    True,
    Palette256,
    None,
}

impl ColorDepth {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "truecolor" | "24bit" | "true" => Some(ColorDepth::True),
            "256" | "xterm" => Some(ColorDepth::Palette256),
            "none" | "off" | "plain" => Some(ColorDepth::None),
            _ => None,
        }
    }

    /// 自动判断：只有明确声明支持真彩色时才用 24bit，否则退回 256 色。
    pub fn auto() -> Self {
        if cfg!(windows) {
            // Windows Terminal / 支持 VT 的 conhost 都有 24 位色
            return ColorDepth::True;
        }
        match std::env::var("COLORTERM") {
            Ok(v) if v.contains("truecolor") || v.contains("24bit") => ColorDepth::True,
            _ => ColorDepth::Palette256,
        }
    }
}

/// xterm-256 调色板量化：6x6x6 色立方 + 24 级灰阶，取更近的那个。
///
/// 灰阶候选用 **RGB 均值**做闭式解（不是 Rec.709 亮度，否则不是最近邻）。
fn quantize_256(rgb: [u8; 3]) -> u8 {
    const LV: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let mut ci = [0usize; 3];
    let mut cube = [0i32; 3];
    for k in 0..3 {
        let v = rgb[k] as i32;
        let mut best = 0usize;
        let mut bd = i32::MAX;
        for (i, lv) in LV.iter().enumerate() {
            let d = (v - lv).abs();
            if d < bd {
                bd = d;
                best = i;
            }
        }
        ci[k] = best;
        cube[k] = LV[best];
    }
    let cube_idx = 16 + 36 * ci[0] as i32 + 6 * ci[1] as i32 + ci[2] as i32;

    let mean = (rgb[0] as f32 + rgb[1] as f32 + rgb[2] as f32) / 3.0;
    let gi = (((mean - 8.0) / 10.0).round() as i32).clamp(0, 23);
    let gray = 8 + 10 * gi;

    let d_cube: i32 = (0..3).map(|k| {
        let d = rgb[k] as i32 - cube[k];
        d * d
    }).sum();
    let d_gray: i32 = (0..3).map(|k| {
        let d = rgb[k] as i32 - gray;
        d * d
    }).sum();

    if d_cube <= d_gray {
        cube_idx as u8
    } else {
        (232 + gi) as u8
    }
}

pub struct Painter {
    prev: Option<Canvas>,
    depth: ColorDepth,
    /// 每个字符实际占几列（1=普通；2=终端把歧义宽度字符当双宽）
    cell_width: usize,
}

impl Painter {
    pub fn new(depth: ColorDepth, cell_width: usize) -> Self {
        Self {
            prev: None,
            depth,
            cell_width: cell_width.max(1),
        }
    }

    /// 进入备用屏、隐藏光标、清屏。
    pub fn start<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        write!(out, "{ESC}?1049h{ESC}?25l{ESC}2J{ESC}H")?;
        out.flush()
    }

    /// 恢复光标、回到主屏。
    pub fn finish<W: Write>(&mut self, out: &mut W) -> io::Result<()> {
        write!(out, "{ESC}?25h{ESC}?1049l")?;
        out.write_all(b"\n")?;
        out.flush()
    }

    fn differs(prev: Option<&Canvas>, c: &Canvas, i: usize) -> bool {
        match prev {
            None => true,
            Some(p) => {
                let bg_diff = match (&p.bg, &c.bg) {
                    (Some(a), Some(b)) => a[i] != b[i],
                    (None, None) => false,
                    _ => true,
                };
                p.ch[i] != c.ch[i] || p.fg[i] != c.fg[i] || bg_diff
            }
        }
    }

    pub fn draw<W: Write>(&mut self, out: &mut W, c: &Canvas) -> io::Result<()> {
        let prev = self
            .prev
            .as_ref()
            .filter(|p| p.cols == c.cols && p.rows == c.rows);

        let mut s = String::with_capacity(32 * 1024);
        s.push_str(ESC);
        s.push_str("?2026h"); // 同步输出：整帧原子提交

        if prev.is_none() {
            s.push_str(ESC);
            s.push('2');
            s.push('J');
        }

        for r in 0..c.rows {
            let mut col = 0;
            while col < c.cols {
                if !Self::differs(prev, c, r * c.cols + col) {
                    col += 1;
                    continue;
                }
                let start = col;
                while col < c.cols && Self::differs(prev, c, r * c.cols + col) {
                    col += 1;
                }
                self.emit_run(&mut s, c, r, start, col - 1);
            }
        }

        s.push_str(ESC);
        s.push_str("?2026l");
        out.write_all(s.as_bytes())?;
        out.flush()?;
        self.prev = Some(c.clone());
        Ok(())
    }

    fn emit_run(&self, s: &mut String, c: &Canvas, r: usize, from: usize, to: usize) {
        s.push_str(ESC);
        s.push_str(&(r + 1).to_string());
        s.push(';');
        s.push_str(&(from * self.cell_width + 1).to_string());
        s.push('H');

        let mut cur_fg: Option<[u8; 3]> = None;
        let mut cur_bg: Option<[u8; 3]> = None;
        for col in from..=to {
            let i = r * c.cols + col;
            match self.depth {
                ColorDepth::None => {}
                ColorDepth::True => {
                    let fg = c.fg[i];
                    if cur_fg != Some(fg) {
                        s.push_str(ESC);
                        s.push_str(&format!("38;2;{};{};{}m", fg[0], fg[1], fg[2]));
                        cur_fg = Some(fg);
                    }
                    if let Some(bgs) = &c.bg {
                        let bg = bgs[i];
                        if cur_bg != Some(bg) {
                            s.push_str(ESC);
                            s.push_str(&format!("48;2;{};{};{}m", bg[0], bg[1], bg[2]));
                            cur_bg = Some(bg);
                        }
                    }
                }
                ColorDepth::Palette256 => {
                    let fg = c.fg[i];
                    if cur_fg != Some(fg) {
                        s.push_str(ESC);
                        s.push_str(&format!("38;5;{}m", quantize_256(fg)));
                        cur_fg = Some(fg);
                    }
                    if let Some(bgs) = &c.bg {
                        let bg = bgs[i];
                        if cur_bg != Some(bg) {
                            s.push_str(ESC);
                            s.push_str(&format!("48;5;{}m", quantize_256(bg)));
                            cur_bg = Some(bg);
                        }
                    }
                }
            }
            s.push(c.ch[i]);
        }
    }
}
