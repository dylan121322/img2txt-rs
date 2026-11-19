# img2txt-rs

实时摄像头 / 图片 → 终端字符画的 **Rust 重写版**，主打 Windows 双击即用。

## 项目来源

本项目是 **[yaochao/img_to_txt](https://github.com/yaochao/img_to_txt)**（原作者 yaochao，2018）的重写。
原项目用 Python 实现「图像 → 字符画」，核心原理是：把每个字符格覆盖的像素块取平均，
再把平均亮度量化成视觉密度相近的字符。

原项目的问题（我在分析时逐条复现过，详见[改动说明](#相对原版的改动)）：
`sum(list[np.uint8])` 在 numpy ≥ 2.0 会按 uint8 回绕（NEP 50），导致输出几乎全是 `#`；
依赖钉死在 2018 年版本，现代 Python 装不上；没有颜色；视频模式只是逐帧往下滚。

**许可证沿用原项目的 MIT**，见 [LICENSE](LICENSE)。

## 相对原版的改动

| | 原版（Python, 2018） | 本版（Rust） |
|---|---|---|
| 数值正确性 | `sum(np.uint8)` 在 numpy≥2.0 回绕 → 全 `#` | f64 累加，无溢出 |
| 分辨率 | 1 子像素/字符格 | `half` 2 个（`▀` 上下半块）、`braille` 8 个（2×4 点阵） |
| 颜色 | 无 | 24bit 真彩色 |
| 终端绘制 | 每帧 `print` 往下滚 | 备用屏 + **绝对定位** + 增量刷新 + 同步输出（不频闪） |
| 运行时 | 要 Python + venv + 一堆 pin 死的包 | **单个 exe，双击即用** |
| 依赖 | numpy / Pillow / requests / opencv | 纯 Rust：`nokhwa`(摄像头) + `ctrlc` + `terminal_size` |
| 构建 | 各平台自己装环境 | GitHub Actions 原生 Windows 构建 |

### 为什么「不频闪」值得单独说

早期 Python 版用相对光标回退（`ESC[{n}F`）来回原帧起点。只要有一帧比可视区高、
或终端把某行多折了一行，相对位移就会**累积漂移** —— 画面被切成几段、下方大片空白。
本版的四条保障：

1. **备用屏** `ESC[?1049h/l`：独占一块屏，退出后原样还原
2. **绝对定位** `ESC[{row};{col}H`：每帧落在确定坐标，物理上不可能漂
3. **增量刷新**：只重画变化的格子
4. **同步输出** `ESC[?2026h/l`：整帧原子提交，看不到半帧

外加**按终端高度自动收窄列数**，保证画面永远不触发滚动。

## 用法

```text
img2txt                     # 摄像头 0，实时模式（双击 exe 即可）
img2txt --camera 1          # 换一个摄像头
img2txt -w 80 -m ascii      # 80 列、纯字符
img2txt -m braille          # 盲文点阵，单色下细节最多
img2txt --no-color          # 关闭颜色
img2txt --list              # 列出可用摄像头
img2txt -h                  # 帮助
```

运行中按 `Ctrl-C` 退出，终端会自动恢复。

**Windows 注意**：建议用 Windows Terminal，双击 exe 会打开控制台窗口；
出错时会停住并提示「按回车键退出」，不会一闪而过。

## 构建

### 用 CI 构建（推荐，无需本地环境）

推到 `main` 分支或手动触发 `workflow_dispatch`，Actions 会产出
`img2txt-windows-x64` artifact；打 `v*` 标签则自动附加到 Release。

### 本地构建

```bash
cargo build --release
# 产物：target/release/img2txt（Windows 为 img2txt.exe）
```

需要 Rust 1.85+（edition 2021）。摄像头依赖是纯 Rust 的 `nokhwa`，
**不需要 OpenCV**，所以不需要在各平台装原生库。

## 参数

| 参数 | 说明 |
|---|---|
| `-c, --camera <N>` | 摄像头索引，默认 0 |
| `-w, --width <N>` | 输出列数，默认跟随终端宽度（上限 100） |
| `-m, --mode <M>` | `half`（默认）/ `ascii` / `braille` |
| `--no-color` | 关闭颜色；`half` 会自动降级成 `ascii`（它的信息全在颜色里） |
| `--list` | 列出摄像头后退出 |

## License

MIT，沿用自 [yaochao/img_to_txt](https://github.com/yaochao/img_to_txt)。
