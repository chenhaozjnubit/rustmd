//! Headless performance harness: `rustmd --bench <file.md>`.
//!
//! Opening a document has three costs that have to stay bounded as the file
//! grows: parsing (once), layout (every frame), and the whole-document scans
//! that feed the status bar, the outline and the find bar. This tool measures
//! each of them on a real file so the large-file work can be judged against
//! numbers rather than impressions.

use std::time::{Duration, Instant};

use crate::doc::Document;
use crate::editor::{Editor, EditorCtx};
use crate::render::RenderCtx;
use crate::theme::Theme;

pub fn run(args: &[String]) {
    let Some(input) = args.first() else {
        eprintln!("用法：rustmd --bench <输入.md>");
        std::process::exit(2);
    };
    let path = std::path::PathBuf::from(input);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("无法读取 {}：{e}", path.display());
            std::process::exit(1);
        }
    };
    let mb = bytes.len() as f64 / (1024.0 * 1024.0);
    println!("文件：{}  {:.2} MB", path.display(), mb);

    let t = Instant::now();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    println!("  UTF-8 解码        {:>9.2} ms", ms(t.elapsed()));
    drop(bytes);

    let t = Instant::now();
    let mut doc = Document::from_text(text, None);
    println!(
        "  解析（块 + 大纲） {:>9.2} ms   {} 个块",
        ms(t.elapsed()),
        doc.parsed.blocks.len()
    );
    println!("  常驻内存（解析后）{:>9.1} MB", rss_mb());

    // Whole-document scans that the UI used to run every frame.
    let t = Instant::now();
    let _ = doc.stats();
    println!("  stats()           {:>9.2} ms", ms(t.elapsed()));
    let t = Instant::now();
    let o = doc.outline();
    println!("  outline()         {:>9.2} ms   {} 项", ms(t.elapsed()), o.len());

    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    Theme::light().apply(&ctx);
    let ec = EditorCtx {
        theme: Theme::light(),
        base: 16.5,
        line_height: 1.72,
        content_width: 780.0,
        wrap_code: false,
        focus_mode: false,
        typewriter: false,
        doc_dir: None,
    };
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 820.0),
        )),
        ..Default::default()
    };

    // What one editor frame costs. The first frame of a fresh editor has no
    // heights yet and has to lay the whole document out; every frame after it
    // draws only what is on screen, so the two are reported separately.
    let mut ed = Editor::default();
    let warm = Instant::now();
    let _ = ctx.run(input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| ed.show(ui, &mut doc, &ec));
    });
    println!("  首帧（含字体预热）{:>9.2} ms", ms(warm.elapsed()));
    let mut worst = 0.0f64;
    let frames = 30;
    let mut samples: Vec<f64> = Vec::with_capacity(frames);
    let total = Instant::now();
    for _ in 0..frames {
        let f = Instant::now();
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ed.show(ui, &mut doc, &ec));
        });
        let el = ms(f.elapsed());
        samples.push(el);
        worst = worst.max(el);
    }
    let avg = ms(total.elapsed()) / frames as f64;
    let mut sorted = samples.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    println!(
        "  编辑帧（视口渲染）{:>9.2} ms 平均 / {:.2} ms 中位 / {:.2} ms 最差   → {:.1} fps   (每帧画 {} 块)",
        avg,
        sorted[sorted.len() / 2],
        worst,
        1000.0 / avg.max(0.001),
        ed.drawn_blocks()
    );

    // The same for reading mode, which renders every block and never has an
    // editable one.
    let mut ed2 = Editor::default();
    ed2.reading = true;
    let mut worst2 = 0.0f64;
    let total2 = Instant::now();
    for _ in 0..frames {
        let f = Instant::now();
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| ed2.show(ui, &mut doc, &ec));
        });
        worst2 = worst2.max(ms(f.elapsed()));
    }
    let avg2 = ms(total2.elapsed()) / frames as f64;
    println!(
        "  阅读帧（视口渲染）{:>9.2} ms 平均 / {:.2} ms 最差   (每帧画 {} 块)",
        avg2,
        worst2,
        ed2.drawn_blocks()
    );
    println!("  常驻内存（渲染后）{:>9.1} MB", rss_mb());

    // Raw layout throughput, so the budget for a screenful can be reasoned about.
    let parsed = doc.parsed.clone();
    let rctx = RenderCtx {
        parsed: parsed.clone(),
        theme: &ec.theme,
        base: 16.5,
        line_height: 1.72,
        content_width: 780.0,
        wrap_code: false,
        doc_dir: None,
        active: None,
        focus_mode: false,
    };
    let t = Instant::now();
    let mut laid = 0usize;
    let mut ui_holder: Option<f32> = None;
    let _ = ctx.run(input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let n = parsed.blocks.len().min(300);
            for i in 0..n {
                let r = crate::render::render_block(ui, &rctx, i);
                laid += 1;
                ui_holder = Some(r.rect.bottom());
            }
        });
    });
    let el = ms(t.elapsed());
    println!(
        "  裸排版 {} 块      {:>9.2} ms   → {:.3} ms/块  (视口约 30 块 = {:.2} ms)",
        laid,
        el,
        el / laid.max(1) as f64,
        el / laid.max(1) as f64 * 30.0
    );
    let _ = ui_holder;

    let _ = Duration::from_millis(0);
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Peak resident set size of this process, in MB.
///
/// Asked of the kernel rather than of `ps`: spawning a process for one number
/// is a lot of work, and on a locked-down machine `ps` may simply not be
/// allowed. The peak is the interesting figure anyway — the question a large
/// file raises is whether the process grows without bound, not what it happens
/// to be holding at the moment it is asked.
#[cfg(unix)]
fn rss_mb() -> f64 {
    // SAFETY: `getrusage` fills the struct it is handed, and `RUSAGE_SELF`
    // needs no other setup. Zeroing first matters: macOS reports the peak only
    // when the field starts at zero.
    unsafe {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return 0.0;
        }
        let peak = usage.assume_init().ru_maxrss as f64;
        // macOS counts bytes, everyone else counts kilobytes.
        #[cfg(target_os = "macos")]
        {
            peak / (1024.0 * 1024.0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            peak / 1024.0
        }
    }
}

#[cfg(not(unix))]
fn rss_mb() -> f64 {
    0.0
}
