//! img2txt —— 实时摄像头 / 图片 → 终端字符画。
//!
//! 无参数运行时直接进入摄像头实时模式（双击 exe 即可用）。

mod render;
mod term;

use std::io;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{CameraIndex, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;

use render::Mode;
use term::Painter;

static STOP: AtomicBool = AtomicBool::new(false);

const USAGE: &str = "\
img2txt —— 实时摄像头 / 图片 → 终端字符画

用法:
  img2txt                     # 默认：摄像头 0，实时模式（双击即可）
  img2txt --camera 1          # 换一个摄像头
  img2txt -w 80 -m ascii      # 80 列、纯字符
  img2txt --list              # 列出可用摄像头

选项:
  -c, --camera <N>   摄像头索引（默认 0）
  -w, --width <N>    输出列数（默认跟随终端宽度，上限 100）
  -m, --mode <M>     half(默认) | ascii | braille
      --no-color     关闭颜色（half 会降级成 ascii）
      --list         列出摄像头后退出
  -h, --help         显示帮助
  -V, --version      显示版本

原理与改动见 README（源自 yaochao/img_to_txt，MIT）。
";

struct Args {
    camera: u32,
    width: Option<usize>,
    mode: Mode,
    color: bool,
    list: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        camera: 0,
        width: None,
        mode: Mode::Half,
        color: true,
        list: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("img2txt {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--list" => a.list = true,
            "--no-color" => a.color = false,
            "-c" | "--camera" => {
                let v = it.next().ok_or("--camera 需要一个数字")?;
                a.camera = v.parse().map_err(|_| format!("摄像头索引不是数字：{v}"))?;
            }
            "-w" | "--width" => {
                let v = it.next().ok_or("--width 需要一个数字")?;
                let n: usize = v.parse().map_err(|_| format!("宽度不是数字：{v}"))?;
                if n == 0 {
                    return Err("宽度必须大于 0".into());
                }
                a.width = Some(n);
            }
            "-m" | "--mode" => {
                let v = it.next().ok_or("--mode 需要一个值")?;
                a.mode = Mode::parse(&v)
                    .ok_or_else(|| format!("未知模式：{v}（可用 half / ascii / braille）"))?;
            }
            other => return Err(format!("未知参数：{other}\n\n{USAGE}")),
        }
    }
    if !a.color && a.mode == Mode::Half {
        // half 的信息全在颜色里（字符恒为 ▀），无色时必须降级
        a.mode = Mode::Ascii;
    }
    Ok(a)
}

#[cfg(windows)]
fn platform_init() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, SetConsoleOutputCP,
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let _ = SetConsoleOutputCP(65001); // UTF-8，否则 ▀ / 盲文会乱码
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            let _ = SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn platform_init() {}

/// 双击运行时窗口会一闪而过，出错时停一下让人看清。
fn pause_on_error() {
    use std::io::IsTerminal;
    if io::stdin().is_terminal() {
        eprintln!("\n按回车键退出…");
        let mut buf = String::new();
        let _ = io::stdin().read_line(&mut buf);
    }
}

fn main() -> ExitCode {
    platform_init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("\n错误：{e}");
            pause_on_error();
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    if args.list {
        let backend = nokhwa::native_api_backend().ok_or("没有可用的摄像头后端")?;
        let devs = nokhwa::query(backend).map_err(|e| format!("枚举摄像头失败：{e}"))?;
        if devs.is_empty() {
            println!("没有找到摄像头");
        }
        for d in devs {
            println!("{:?}  {}", d.index(), d.human_name());
        }
        return Ok(());
    }

    ctrlc::set_handler(|| STOP.store(true, Ordering::SeqCst))
        .map_err(|e| format!("安装 Ctrl-C 处理器失败：{e}"))?;

    let mut cam = Camera::new(
        CameraIndex::Index(args.camera),
        RequestedFormat::new::<RgbFormat>(RequestedFormatType::AbsoluteHighestFrameRate),
    )
    .map_err(|e| {
        format!(
            "打不开摄像头 {}：{e}\n  提示：换个索引试试 img2txt --camera 1，或用 img2txt --list 查看"
            , args.camera
        )
    })?;

    cam.open_stream()
        .map_err(|e| format!("摄像头开流失败：{e}"))?;

    let fps = (cam.frame_rate()).max(1) as f64;
    let interval = Duration::from_secs_f64(1.0 / fps);

    let first = cam.frame().map_err(|e| format!("读取首帧失败：{e}"))?;
    let res = first.resolution();
    let (w, h) = (res.width_x as usize, res.height_y as usize);
    if w == 0 || h == 0 {
        return Err("摄像头返回了 0 尺寸的画面".into());
    }

    // 输出宽度：跟随终端，并保证不超出终端高度（超出会触发滚动 → 画面跳动）
    let (term_w, term_h) = terminal_size::terminal_size()
        .map(|(tw, th)| (tw.0 as usize, th.0 as usize))
        .unwrap_or((80, 24));

    let mut cols = args.width.unwrap_or_else(|| term_w.min(100));
    let max_rows = term_h.saturating_sub(1).max(4);
    let max_cols = ((max_rows as f64 * 2.0 * w as f64 / h as f64) as usize).max(20);
    if cols > max_cols {
        cols = max_cols;
    }

    eprintln!(
        "img2txt {} · 摄像头 {} · {}x{} → {} 列 / {} 行 · {} fps · {} · Ctrl-C 退出",
        env!("CARGO_PKG_VERSION"),
        args.camera,
        w,
        h,
        cols,
        render::rows_for(cols, w, h),
        fps as u32,
        args.mode.name()
    );

    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut painter = Painter::new(args.color);
    painter.start(&mut out).map_err(io_msg)?;

    let mut raw = vec![0u8; w * h * 3];
    let mut next = Instant::now();
    let mut first_frame = Some(first);

    let result = loop {
        if STOP.load(Ordering::SeqCst) {
            break Ok(());
        }
        let buf = match first_frame.take() {
            Some(f) => f,
            None => match cam.frame() {
                Ok(f) => f,
                Err(e) => break Err(format!("读取画面失败：{e}")),
            },
        };
        let r = buf.resolution();
        let (fw, fh) = (r.width_x as usize, r.height_y as usize);
        if fw != w || fh != h {
            break Err("摄像头分辨率中途变化，暂不支持".into());
        }
        if let Err(e) = buf.decode_image_to_buffer::<RgbFormat>(&mut raw) {
            break Err(format!("帧解码失败：{e}"));
        }

        let canvas = render::render(&raw, w, h, cols, args.mode);
        if let Err(e) = painter.draw(&mut out, &canvas) {
            break Err(io_msg(e));
        }

        next += interval;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now.duration_since(next) > interval {
            next = now;
        }
    };

    let _ = painter.finish(&mut out);
    let _ = cam.stop_stream();
    result
}

fn io_msg(e: io::Error) -> String {
    format!("写入终端失败：{e}")
}
