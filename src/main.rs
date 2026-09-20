//! rustmd — a Typora-flavoured Markdown editor and reader, written in Rust.
//!
//! The whole application is a single `egui` viewport. `App` owns the document
//! and every command; `editor` implements the live-preview widget and `render`
//! draws the blocks that are not currently being edited.
//!
//! Running with `--export <in.md> [out.html]` skips the GUI and writes the
//! standalone HTML rendering instead, which is handy for scripts.
//!
//! Running with `--bench <in.md>` measures the parse and layout cost of a
//! document without opening a window, which is how the large-file budget was
//! established.

// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bench;
mod code_hl;
mod config;
mod doc;
mod editor;
mod fonts;
mod html_export;
mod math;
mod html;
mod mermaid;
mod panels;
mod parser;
mod picker;
mod render;
mod theme;

/// Documents handed over by Finder: on macOS they arrive as Apple Events, on
/// every other platform there is simply no such channel.
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
mod macos {
    use std::path::PathBuf;
    pub fn install() {}
    pub fn take_opened() -> Vec<PathBuf> {
        Vec::new()
    }
}

#[cfg(test)]
mod smoke;

use std::path::PathBuf;

use app::App;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--export") => {
            export(&args[1..]);
            return Ok(());
        }
        Some("--bench") => {
            bench::run(&args[1..]);
            return Ok(());
        }
        Some("--help" | "-h") => {
            print_help();
            return Ok(());
        }
        _ => {}
    }

    // Skip anything that still looks like a switch. Launch Services hands a
    // bundled app a `-psn_…` argument, which is not a document.
    let startup = args
        .into_iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from);

    run_gui(startup)
}

fn run_gui(startup: Option<PathBuf>) -> eframe::Result<()> {
    let cfg = config::Config::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("rustmd")
        .with_inner_size([1120.0, 780.0])
        .with_min_inner_size([560.0, 400.0])
        .with_app_id("rustmd")
        // 必须显式给图标：eframe 在缺省时会用自带的 egui logo（黑底白 "e"）
        // 在运行时调 -[NSApplication setApplicationIconImage:]，把 bundle 里
        // 的 icns 顶掉 —— Finder 看到的还是对的，Dock 上却是 egui 的 "e"。
        .with_icon(app_icon());
    if let Some((x, y, w, h)) = cfg.window {
        viewport = viewport.with_position([x, y]).with_inner_size([w, h]);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    // A path on the command line opens straight away.
    let startup = startup.filter(|p| p.exists());

    eframe::run_native(
        "rustmd",
        options,
        Box::new(move |cc| {
            let mut app = App::new(cc);
            if let Some(path) = startup.clone() {
                app.open_startup_file(&path);
            }
            Ok(Box::new(app))
        }),
    )
}

/// 把 `assets/icon-256.png` 编译进二进制，作为运行时的应用图标。
/// 用 `image` 解码成 RGBA，交给 eframe 设置到 NSApplication 上。
fn app_icon() -> std::sync::Arc<egui::IconData> {
    let png = include_bytes!("../assets/icon-256.png");
    let img = image::load_from_memory(png)
        .expect("assets/icon-256.png 是构建产物，应当总能解码")
        .into_rgba8();
    let (width, height) = img.dimensions();
    std::sync::Arc::new(egui::IconData {
        width,
        height,
        rgba: img.into_raw(),
    })
}

/// `rustmd --export input.md [output.html]`
fn export(rest: &[String]) {
    let Some(input) = rest.first() else {
        eprintln!("用法：rustmd --export <输入.md> [输出.html]");
        std::process::exit(2);
    };
    let input_path = PathBuf::from(input);
    let text = match std::fs::read(&input_path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            eprintln!("无法读取 {}：{e}", input_path.display());
            std::process::exit(1);
        }
    };
    let stem = input_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "未命名".into());
    let out = rest
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| input_path.with_extension("html"));

    let html = html_export::to_html(&text, &stem, false);
    match std::fs::write(&out, html) {
        Ok(()) => println!("已导出 {}", out.display()),
        Err(e) => {
            eprintln!("写入 {} 失败：{e}", out.display());
            std::process::exit(1);
        }
    }
}

fn print_help() {
    println!(
        "rustmd — Rust 编写的 Markdown 编辑器 / 阅读器\n\n\
         用法：\n\
         \x20 rustmd [文件.md]                        打开编辑器\n\
         \x20 rustmd --export <输入.md> [输出.html]   仅导出 HTML\n\
         \x20 rustmd --help                          显示本帮助\n"
    );
}
