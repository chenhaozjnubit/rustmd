//! Headless smoke tests.
//!
//! `egui` can run without a window, so the whole render and editor path can be
//! exercised in CI. These tests do not check pixels — they check that every
//! block kind survives layout, that the editor widget copes with the caret on
//! every block, and that editing round-trips through the document.

use crate::doc::Document;
use crate::editor::{Editor, EditorCtx};
use crate::theme::Theme;

/// The sample document doubles as the render-path fixture.
const SAMPLE: &str = include_str!("../samples/demo.md");

/// The stress document: Mermaid, LaTeX and raw HTML, all in one file.
const STRESS: &str = include_str!("../samples/test-render.md");

fn headless_ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::fonts::install(&ctx);
    Theme::light().apply(&ctx);
    ctx
}

fn input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 820.0),
        )),
        ..Default::default()
    }
}

fn editor_ctx(base: f32) -> EditorCtx {
    EditorCtx {
        theme: Theme::light(),
        base,
        line_height: 1.72,
        content_width: 780.0,
        wrap_code: false,
        focus_mode: false,
        typewriter: false,
        doc_dir: None,
    }
}

/// Sample points on `screen` that no opaque rectangle covers.
///
/// A background shape in egui is a rectangle with an opaque fill; a pixel that
/// none of them covers is a pixel the app never painted. What shows there is
/// the window's clear colour, which on macOS composites to black — so a gap
/// here is a black bar on screen, which is exactly the failure this guards.
fn uncovered(out: &egui::FullOutput, screen: egui::Rect, step: f32) -> Vec<egui::Pos2> {
    let mut painted: Vec<egui::Rect> = Vec::new();
    for cs in &out.shapes {
        let egui::Shape::Rect(r) = &cs.shape else {
            continue;
        };
        if r.fill.a() != 255 {
            continue;
        }
        let vis = r.rect.intersect(cs.clip_rect);
        if vis.width() > 0.0 && vis.height() > 0.0 {
            painted.push(vis);
        }
    }

    let mut holes = Vec::new();
    let mut y = screen.top() + step * 0.5;
    while y < screen.bottom() {
        let mut x = screen.left() + step * 0.5;
        while x < screen.right() {
            let p = egui::pos2(x, y);
            if !painted.iter().any(|r| r.contains(p)) {
                holes.push(p);
            }
            x += step;
        }
        y += step;
    }
    holes
}

/// A directory whose entry names are long enough to overflow a side panel.
///
/// One long name used to widen the panel's *content*, and a panel paints its
/// background over its content's used rect — so the fill stopped at the panel's
/// clipped edge while the panel still reserved the wider rect. The strip in
/// between was painted by nobody and showed the window background through the
/// transparent clear: the black bar. This fixture is the regression guard.
fn tree_with_long_names() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("rustmd-smoke-long-names");
    let nested = dir.join("一个层级很深而且名字非常非常长长长长的子目录");
    let _ = std::fs::create_dir_all(&nested);
    let long = "很长的文件名".repeat(10);
    for name in [format!("{long}.md"), "普通.md".to_string()] {
        let _ = std::fs::write(dir.join(&name), "# 标题\n\n正文。\n");
        let _ = std::fs::write(nested.join(&name), "# 标题\n\n正文。\n");
    }
    dir
}

#[test]
fn every_part_of_the_window_gets_painted() {
    use crate::app::Pane;
    use crate::config::Config;

    let wide = Config {
        sidebar_width: 460.0,
        ..Default::default()
    };
    let samples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let long_names = tree_with_long_names();
    let cases = [
        (
            "实时预览 · 默认设置 · 1200×820",
            Config::default(),
            Pane::Edit,
            (1200.0, 820.0),
            samples.clone(),
        ),
        (
            // The widths from the screenshot: a sidebar dragged out to its
            // maximum, which is the widest the columns can get.
            "实时预览 · 宽侧栏 · 2499×1328",
            wide.clone(),
            Pane::Edit,
            (2499.0, 1328.0),
            samples.clone(),
        ),
        (
            "源码 · 宽侧栏 · 2499×1328",
            wide.clone(),
            Pane::Source,
            (2499.0, 1328.0),
            samples.clone(),
        ),
        (
            "分栏 · 宽侧栏 · 2499×1328",
            wide.clone(),
            Pane::Split,
            (2499.0, 1328.0),
            samples.clone(),
        ),
        (
            "实时预览 · 窄窗口 · 800×560",
            Config::default(),
            Pane::Edit,
            (800.0, 560.0),
            samples.clone(),
        ),
        (
            // The reported bug: a long entry name in the file tree.
            "实时预览 · 超长文件名 · 2500×1420",
            wide.clone(),
            Pane::Edit,
            (2500.0, 1420.0),
            long_names.clone(),
        ),
        (
            "实时预览 · 超长文件名 · 窄侧栏 · 1120×780",
            Config::default(),
            Pane::Edit,
            (1120.0, 780.0),
            long_names.clone(),
        ),
    ];

    for (name, cfg, pane, (w, h), tree) in cases {
        let ctx = headless_ctx();
        let mut app = crate::app::App::with_ctx_and_config(&ctx, cfg);
        app.set_pane(pane);
        // Point the file tree at a real directory: an empty panel says much
        // less about the layout than a populated one. `open_path` on a
        // directory only fills the tree — it does not touch the config file.
        app.open_path(&tree);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h));

        let mut out = None;
        for _ in 0..3 {
            out = Some(ctx.run(
                egui::RawInput {
                    screen_rect: Some(screen),
                    ..Default::default()
                },
                |ctx| app.ui(ctx),
            ));
        }
        let out = out.expect("three frames ran");

        let holes = uncovered(&out, screen, 12.0);
        let samples = ((w / 12.0).ceil() * (h / 12.0).ceil()) as usize;
        assert!(
            holes.is_empty(),
            "{name}：{}/{} 个采样点从未被任何背景矩形覆盖，例如 {:?}",
            holes.len(),
            samples,
            &holes[..holes.len().min(8)]
        );
    }
}

#[test]
fn lays_out_every_block_of_the_sample() {
    let ctx = headless_ctx();
    let mut doc = Document::from_text(SAMPLE, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);

    assert!(
        doc.parsed.blocks.len() > 25,
        "sample should exercise many blocks, got {}",
        doc.parsed.blocks.len()
    );

    for _ in 0..2 {
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.show(ui, &mut doc, &ec);
            });
        });
    }
}

/// `samples/test-render.md` is what gets opened when someone wants to see
/// whether Mermaid, LaTeX and raw HTML survive. It has to survive here first.
#[test]
fn the_stress_document_lays_out() {
    let ctx = headless_ctx();
    let mut doc = Document::from_text(STRESS, None);
    let mut ed = Editor::default();
    ed.reading = true;
    let ec = editor_ctx(16.5);

    assert!(
        doc.parsed.blocks.len() > 40,
        "the stress doc should be dense, got {} blocks",
        doc.parsed.blocks.len()
    );

    for _ in 0..2 {
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.show(ui, &mut doc, &ec);
            });
        });
    }
}

/// Same document, but with the caret parked on every block in turn — the
/// editing path and the reading path draw Mermaid differently, and both have
/// to hold.
#[test]
fn the_stress_document_survives_the_caret_everywhere() {
    let ctx = headless_ctx();
    let mut doc = Document::from_text(STRESS, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    let n = doc.parsed.blocks.len();

    for i in 0..n {
        let at = doc.parsed.blocks[i].range.start;
        ed.set_caret(at, Some(i));
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ed.show(ui, &mut doc, &ec);
                });
            });
        }
    }
}

#[test]
fn survives_the_caret_landing_on_every_block() {
    let ctx = headless_ctx();
    let mut doc = Document::from_text(SAMPLE, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    let n = doc.parsed.blocks.len();

    for i in 0..n {
        let at = doc.parsed.blocks[i].range.start;
        ed.set_caret(at, Some(i));
        for _ in 0..2 {
            let _ = ctx.run(input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ed.show(ui, &mut doc, &ec);
                });
            });
        }
    }
}

#[test]
fn reader_mode_lays_out_everything_unattended() {
    let ctx = headless_ctx();
    let mut doc = Document::from_text(SAMPLE, None);
    let mut ed = Editor::default();
    ed.reading = true;
    let ec = editor_ctx(14.0);
    for _ in 0..2 {
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.show(ui, &mut doc, &ec);
            });
        });
    }
}

/// Run `n` frames of the editor. Blocks are not drawn to a screen, but the
/// whole layout path runs, which is what these tests are for.
fn run_frames(
    ctx: &egui::Context,
    ed: &mut Editor,
    doc: &mut Document,
    ec: &EditorCtx,
    n: usize,
) {
    for _ in 0..n {
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ed.show(ui, doc, ec);
            });
        });
    }
}

#[test]
fn a_large_document_draws_only_what_is_on_screen() {
    let ctx = headless_ctx();
    // Long enough that measuring every block up front would be the wrong
    // trade, and long enough that laying them all out per frame would be
    // obvious.
    let text: String = (0..3000)
        .map(|i| format!("## 第 {i} 节\n\n这是第 {i} 段正文，用来把文档撑大一些。\n\n"))
        .collect();
    let mut doc = Document::from_text(text, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    let total = doc.parsed.blocks.len();
    assert!(total > 3000, "expected a large document, got {total} blocks");

    run_frames(&ctx, &mut ed, &mut doc, &ec, 2);
    assert!(
        ed.drawn_blocks() < 120,
        "a frame drew {} of {total} blocks",
        ed.drawn_blocks()
    );

    // Jumping about must keep the window small, and must keep the scroll
    // position a real number the scrollbar can use.
    for target in [1500, total - 1, 0, total / 3] {
        ed.set_caret(doc.parsed.blocks[target].range.start, Some(target));
        ed.scroll_to_block = Some(target);
        run_frames(&ctx, &mut ed, &mut doc, &ec, 3);
        assert!(
            ed.drawn_blocks() < 120,
            "after jumping to {target} a frame drew {} blocks",
            ed.drawn_blocks()
        );
        assert!(
            ed.scroll_percent.is_finite() && (0.0..=1.0).contains(&ed.scroll_percent),
            "scroll position {target} gave {}",
            ed.scroll_percent
        );
    }
}

#[test]
fn editing_a_large_document_keeps_the_blocks_tiling() {
    let ctx = headless_ctx();
    let text: String = (0..2400)
        .map(|i| format!("第 {i} 段。\n\n"))
        .collect();
    let mut doc = Document::from_text(text, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    run_frames(&ctx, &mut ed, &mut doc, &ec, 2);

    // Type into a block in the middle, then split it in two by pressing Enter.
    // Both change what the height cache has to describe.
    let idx = doc.parsed.blocks.len() / 2;
    let range = doc.parsed.blocks[idx].edit_range(&doc.text);
    doc.apply_block_edit(range, "改过的正文。", 0, 0);
    run_frames(&ctx, &mut ed, &mut doc, &ec, 2);

    let idx = doc.block_at(doc.text.len() / 2);
    let range = doc.parsed.blocks[idx].edit_range(&doc.text);
    let split = format!("{}\n\n新的第二段。", &doc.text[range.clone()]);
    doc.apply_block_edit(range, &split, 0, 0);
    run_frames(&ctx, &mut ed, &mut doc, &ec, 3);

    // Every byte is still in exactly one block, after all that.
    let rebuilt: String = doc
        .parsed
        .blocks
        .iter()
        .map(|b| &doc.text[b.range.clone()])
        .collect();
    assert_eq!(rebuilt, doc.text);
    assert!(ed.drawn_blocks() < 120);
}

#[test]
fn typing_replaces_one_block_and_keeps_the_tiling() {
    let mut doc = Document::from_text("# 标题\n\n正文段落。\n\n- 项\n", None);
    let idx = doc
        .parsed
        .blocks
        .iter()
        .position(|b| matches!(b.kind, crate::parser::BlockKind::Paragraph))
        .expect("paragraph");
    let range = doc.parsed.blocks[idx].edit_range(&doc.text);
    doc.apply_block_edit(range, "改过的正文。", 0, 0);

    assert!(doc.text.contains("改过的正文。"));
    assert!(!doc.text.contains("正文段落。"));
    // Blocks still tile the document exactly.
    let rebuilt: String = doc
        .parsed
        .blocks
        .iter()
        .map(|b| &doc.text[b.range.clone()])
        .collect();
    assert_eq!(rebuilt, doc.text);

    // And the change is undoable.
    doc.undo();
    assert!(doc.text.contains("正文段落。"));
}

/// Where a scrolled frame must put the blocks it draws, and how tall it must
/// make the document.
///
/// Both numbers come from the same place: the height cache. A frame places
/// block `i` at `block_top(i)` in document coordinates, and the scroll area
/// turns that into a screen position by taking the scroll offset off it. So
/// every block a frame draws has to land the *same* distance from its cached
/// position — that distance being the offset — and the document has to be laid
/// out exactly as tall as the cache says it is.
///
/// This is what a band of blank space breaks. The layout position used to be
/// read back from the widget cursor, which is measured from the top of the
/// window and not from the top of the document; the scroll offset then got
/// added a second time as blank space above everything below the first block
/// drawn, so the content stopped moving and the band grew with the offset.
#[test]
fn a_scrolled_frame_keeps_the_blocks_where_the_cache_put_them() {
    let ctx = headless_ctx();
    let text: String = (0..400)
        .map(|i| format!("## 第 {i} 节\n\n这是第 {i} 段正文，用来把文档撑得足够长。\n\n"))
        .collect();
    let mut doc = Document::from_text(text, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    let last = doc.parsed.blocks.len() - 1;

    // The first frames draw the whole document and fill the height cache.
    run_frames(&ctx, &mut ed, &mut doc, &ec, 2);
    let total = ed.document_height();
    assert!(total > 2000.0, "the fixture scrolls: {total}px");

    // Jump down the document, then give the offset frames to settle — a frame
    // that scrolls reports the offset it is about to use, not the one it drew.
    for target in [40, 200, last] {
        ed.set_caret(doc.parsed.blocks[target].range.start, Some(target));
        ed.scroll_to_block = Some(target);
        run_frames(&ctx, &mut ed, &mut doc, &ec, 4);
        check_the_frame_tiles_the_document(&ed, &format!("jump to {target}"));
    }
}

/// The same invariant past the measure limit, where heights start as guesses.
///
/// The blocks above the viewport are then only estimated, and every block that
/// is drawn replaces its estimate — which moves everything below it. The
/// scroll area and the cache still have to agree.
#[test]
fn a_scrolled_frame_tiles_a_document_larger_than_the_measure_limit() {
    let ctx = headless_ctx();
    let text: String = (0..3000)
        .map(|i| format!("## 第 {i} 节\n\n这是第 {i} 段正文，用来把文档撑得足够长。\n\n"))
        .collect();
    let mut doc = Document::from_text(text, None);
    let mut ed = Editor::default();
    let ec = editor_ctx(16.5);
    let last = doc.parsed.blocks.len() - 1;
    assert!(last > 2048, "the fixture is past the measure limit");

    run_frames(&ctx, &mut ed, &mut doc, &ec, 2);

    for target in [2200, 7000, last] {
        ed.set_caret(doc.parsed.blocks[target].range.start, Some(target));
        ed.scroll_to_block = Some(target);
        run_frames(&ctx, &mut ed, &mut doc, &ec, 4);
        check_the_frame_tiles_the_document(&ed, &format!("jump to {target}"));
    }
}

/// Assert what the last frame laid out: the document is exactly as tall as the
/// cache says it is, and every block it drew is the same distance from its
/// cached position — that distance being the scroll offset, and nothing else.
fn check_the_frame_tiles_the_document(ed: &Editor, label: &str) {
    assert!(
        ed.scroll_offset() > 100.0,
        "{label} left the document at {}",
        ed.scroll_offset()
    );

    let laid_out = ed.laid_out_height();
    let total = ed.document_height();
    assert!(
        (laid_out - total).abs() < 1.0,
        "{label}: laid out {laid_out:.1}px for a document the cache says is {total:.1}px"
    );

    let drawn = ed.drawn_layout_tops();
    assert!(drawn.len() > 4, "{label} drew {} blocks", drawn.len());
    let shifts: Vec<f32> = drawn.iter().map(|&(i, top)| top - ed.block_top(i)).collect();
    for ((i, _), shift) in drawn.iter().zip(&shifts) {
        assert!(
            (shift - shifts[0]).abs() < 0.5,
            "{label}: block {i} sits {shift:.1}px away from its cached position \
             while the first drawn block sits {:.1}px",
            shifts[0]
        );
    }
}

/// A `\begin{…}` body must actually stack its rows.
///
/// The regression this pins down: `\\` was not recognised as a row separator,
/// so every matrix, `cases` and `aligned` came out as one long line with a
/// stray `\` painted where the break should have been. Height is the cheapest
/// honest witness — a two-row grid cannot be as short as a one-row one.
#[test]
fn a_math_environment_stacks_its_rows() {
    let ctx = headless_ctx();
    let measure = |tex: &str| -> f32 {
        let mut h = 0.0;
        let _ = ctx.run(input(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                h = crate::math::layout(ui, tex, 18.0, egui::Color32::BLACK, true).height();
            });
        });
        h
    };

    let one_row = measure("\\begin{pmatrix} a & b \\end{pmatrix}");
    let two_rows = measure("\\begin{pmatrix} a & b \\\\ c & d \\end{pmatrix}");
    let cases = measure("\\begin{cases} x^2, & x \\ge 0 \\\\ -x, & x < 0 \\end{cases}");
    let aligned =
        measure("\\begin{aligned} (a+b)^2 &= a^2 + 2ab + b^2 \\\\ (a-b)^2 &= a^2 - 2ab + b^2 \\end{aligned}");

    assert!(one_row > 0.0, "a one-row matrix measured {one_row}px");
    for (label, tall) in [("matrix", two_rows), ("cases", cases), ("aligned", aligned)] {
        assert!(
            tall > one_row * 1.5,
            "{label}: two rows measured {tall:.1}px against {one_row:.1}px for one — \
             the rows are not stacking"
        );
    }
}

/// Section 三 of the stress document is a list of HTML the renderer claims to
/// handle. This walks it: every `BlockKind::Html` in the file has to produce a
/// layout, and the two that carry styling have to paint it.
#[test]
fn the_stress_document_renders_its_html_blocks() {
    use crate::html::{self, Ctx};
    use crate::parser::BlockKind;

    let ctx = headless_ctx();
    let doc = Document::from_text(STRESS, None);
    let theme = Theme::light();

    let html_blocks: Vec<&crate::parser::Block> = doc
        .parsed
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Html))
        .collect();
    assert!(
        html_blocks.len() >= 8,
        "the stress document should exercise HTML blocks, found {}",
        html_blocks.len()
    );

    let mut heights = Vec::new();
    let mut fills = Vec::new();
    let out = ctx.run(input(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            for b in &html_blocks {
                let c = Ctx {
                    theme: &theme,
                    defs: &doc.parsed.defs,
                    doc_dir: None,
                    size: 16.0,
                    line_height: 1.7,
                    color: theme.text,
                };
                heights.push((b.range.start, html::draw(ui, &c, &b.content)));
            }
        });
    });
    for cs in &out.shapes {
        if let egui::epaint::Shape::Rect(r) = &cs.shape {
            if r.fill != egui::Color32::TRANSPARENT {
                fills.push(r.fill);
            }
        }
    }

    for (at, h) in heights {
        let h = h.unwrap_or_else(|| panic!("the HTML block at byte {at} drew nothing"));
        assert!(h > 20.0, "the HTML block at byte {at} is {h}px tall");
    }
    // 3.1's card and 3.6's tinted div, straight out of the document.
    assert!(
        fills.contains(&egui::Color32::from_rgb(0xf4, 0xf7, 0xfb)),
        "the 3.1 card painted no background"
    );
    assert!(
        fills.contains(&egui::Color32::from_rgb(0xff, 0xf8, 0xe1)),
        "the 3.6 callout painted no background"
    );
}

/// The inline half of section 三: `<b>` and friends are markup, so the tags
/// themselves must not reach the page — except for the ones we do not
/// understand, which stay visible on purpose.
#[test]
fn the_stress_document_hides_its_inline_tags() {
    use crate::parser::{parse_inline, InlineCtx, Leaf};

    let doc = Document::from_text(STRESS, None);
    let block = doc
        .parsed
        .blocks
        .iter()
        .find(|b| b.content.contains("<b>粗体 b</b>"))
        .expect("the inline HTML paragraph");

    let leaves = parse_inline(
        &block.content,
        &InlineCtx {
            defs: &doc.parsed.defs,
            keep_markers: false,
        },
    );
    let mut text = String::new();
    for l in &leaves {
        text.push_str(l.plain(&mut String::new()));
    }
    for tag in ["<b>", "</b>", "<i>", "<u>", "<code>", "<span ", "<br"] {
        assert!(!text.contains(tag), "{tag} reached the page: {text}");
    }
    assert!(text.contains("粗体 b"), "the content went missing: {text}");
    assert!(!text.contains("这是一段 HTML 注释"), "a comment was rendered");
    assert!(
        leaves.iter().any(|l| matches!(
            l,
            Leaf::Span { text, style, .. } if text == "粗体 b" && style.strong
        )),
        "`<b>` did not bold its content"
    );
}
