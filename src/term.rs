//! 终端绘制：备用屏 + 绝对定位 + 增量刷新 + 同步输出。
//!
//! 这套机制是解决「频闪」的关键。相对光标回退（`ESC[{n}F`）只要有一帧比可视区高、
//! 或终端多折了一行，位移就会累积漂移 —— 画面被切成几段、下方大片空白。
//! 绝对定位每帧都落在确定坐标，物理上不会漂。

use std::io::{self, Write};

use crate::render::Canvas;

const ESC: &str = "\x1b[";

pub struct Painter {
    prev: Option<Canvas>,
    color: bool,
}

impl Painter {
    pub fn new(color: bool) -> Self {
        Self { prev: None, color }
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
        s.push_str(&(from + 1).to_string());
        s.push('H');

        let mut cur_fg: Option<[u8; 3]> = None;
        let mut cur_bg: Option<[u8; 3]> = None;
        for col in from..=to {
            let i = r * c.cols + col;
            if self.color {
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
            s.push(c.ch[i]);
        }
    }
}
