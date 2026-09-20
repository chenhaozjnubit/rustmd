//! Block rendering.
//!
//! Every block is drawn by composing `egui` widgets and painters:
//!
//! * Text runs go through a `LayoutJob` so a single galley carries bold,
//!   italic, code, highlight, link and strikethrough styling, and can be
//!   measured, hit-tested and mapped back to source offsets.
//! * Segments of a block are laid out with `horizontal_wrapped`, which lets
//!   inline images and inline math flow with the surrounding prose.
//! * Decorative painting (quote bars, list bullets, heading rules, code and
//!   table backgrounds) uses the "shape placeholder" trick: an empty shape is
//!   pushed before the content so it can be filled in afterwards and still
//!   render underneath it.

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{
    Align, Color32, FontId, Galley, Pos2, Rect, Sense, Shape, Stroke, TextFormat, Ui, vec2,
};

use crate::code_hl;
use crate::fonts;
use crate::math;
use crate::parser::{self, Align as ColAlign, Block, BlockKind, Leaf, ListItem, Parsed};
use crate::theme::Theme;

// ===========================================================================
// Hit regions
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HitKind {
    /// A text run: clicks map to a character.
    Text,
    /// An image or formula: clicks map to its start offset.
    Atom,
}

/// A placed piece of a block that maps back to a source offset.
pub struct Hit {
    pub rect: Rect,
    pub galley: Arc<Galley>,
    /// The text that was laid out (identical to the galley's text).
    pub text: String,
    /// (byte range in `text`) -> (byte range in the block's content)
    pub map: Vec<(Range<usize>, Range<usize>)>,
    pub kind: HitKind,
}

impl Hit {
    /// Translate a screen position into an offset in the block's content.
    pub fn content_offset(&self, pos: Pos2) -> usize {
        match self.kind {
            HitKind::Atom => self.map.first().map(|(_, c)| c.start).unwrap_or(0),
            HitKind::Text => {
                let cursor = self.galley.cursor_from_pos(pos - self.rect.min);
                let byte = char_index_to_byte(&self.text, cursor.ccursor.index);
                self.byte_to_content(byte)
            }
        }
    }

    fn byte_to_content(&self, byte: usize) -> usize {
        for (t, c) in &self.map {
            if byte >= t.start && byte <= t.end {
                let d = byte - t.start;
                return c.start + d.min(c.end - c.start);
            }
        }
        self.map
            .last()
            .map(|(_, c)| c.end)
            .or_else(|| self.map.first().map(|(_, c)| c.start))
            .unwrap_or(0)
    }
}

fn char_index_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

pub struct Rendered {
    /// The whole row, used for click-to-focus.
    pub rect: Rect,
    /// Just the content column.
    pub content_rect: Rect,
    pub hits: Vec<Hit>,
    /// Set when the user clicked a task-list checkbox.
    pub toggled_task: Option<usize>,
}

impl Default for Rendered {
    fn default() -> Self {
        Self {
            rect: Rect::NOTHING,
            content_rect: Rect::NOTHING,
            hits: Vec::new(),
            toggled_task: None,
        }
    }
}

pub struct RenderCtx<'a> {
    /// Shared with the document so the editor can render while holding `&mut`.
    pub parsed: Arc<Parsed>,
    pub theme: &'a Theme,
    pub base: f32,
    pub line_height: f32,
    pub content_width: f32,
    pub wrap_code: bool,
    pub doc_dir: Option<PathBuf>,
    /// The block being edited; highlighted differently when focus mode is on.
    pub active: Option<usize>,
    pub focus_mode: bool,
}

/// Point size of a heading, as a multiple of the base size.
///
/// Shared with the editor so that a block's estimated height matches the size
/// it will actually be drawn at.
pub fn heading_size(base: f32, level: u8) -> f32 {
    match level {
        1 => base * 1.86,
        2 => base * 1.5,
        3 => base * 1.26,
        4 => base * 1.12,
        5 => base * 1.0,
        _ => base * 0.92,
    }
}

impl<'a> RenderCtx<'a> {
    pub fn heading_size(&self, level: u8) -> f32 {
        heading_size(self.base, level)
    }

    /// Horizontal space taken by blockquote and list nesting.
    fn indent_of(&self, b: &Block) -> f32 {
        let mut x = b.quote as f32 * 18.0;
        if let BlockKind::ListItem(li) = &b.kind {
            x += li.depth as f32 * 22.0;
        }
        x
    }
}

#[derive(Clone, Copy)]
struct TextStyle {
    size: f32,
    color: Color32,
    bold: bool,
    italic: bool,
    line_height: f32,
}

// ===========================================================================
// Inline layout
// ===========================================================================

enum Chunk<'a> {
    Text(Vec<&'a Leaf>),
    Media(&'a Leaf),
}

fn split_chunks(leaves: &[Leaf]) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut cur: Vec<&Leaf> = Vec::new();
    for l in leaves {
        match l {
            Leaf::Span { .. } => cur.push(l),
            Leaf::Break { hard, .. } => {
                cur.push(l);
                if *hard {
                    out.push(Chunk::Text(std::mem::take(&mut cur)));
                }
            }
            _ => {
                if !cur.is_empty() {
                    out.push(Chunk::Text(std::mem::take(&mut cur)));
                }
                out.push(Chunk::Media(l));
            }
        }
    }
    if !cur.is_empty() {
        out.push(Chunk::Text(cur));
    }
    out
}

/// Text format for one inline span. Shared with the editor so that the block
/// currently being edited is styled exactly like the rendered blocks.
pub fn span_format(
    theme: &Theme,
    size: f32,
    line_height: f32,
    base_bold: bool,
    base_italic: bool,
    style: parser::Style,
    link: Option<&str>,
    default_color: Color32,
) -> TextFormat {
    let bold = base_bold || style.strong;
    let italic = base_italic || style.em;
    let (font_id, color, bg, strike, underline) = if style.marker {
        (
            FontId::new(size * 0.9, fonts::family_for(false, false)),
            theme.marker_dim,
            None,
            false,
            false,
        )
    } else if style.code {
        (
            FontId::new(size * 0.9, fonts::mono_family_for(bold)),
            theme.inline_code_text,
            Some(theme.inline_code_bg),
            false,
            false,
        )
    } else if link.is_some() {
        (
            FontId::new(size, fonts::family_for(bold, italic)),
            theme.link,
            None,
            false,
            true,
        )
    } else if style.highlight {
        (
            FontId::new(size, fonts::family_for(bold, italic)),
            theme.highlight_text,
            Some(theme.highlight_bg),
            false,
            false,
        )
    } else {
        (
            FontId::new(size, fonts::family_for(bold, italic)),
            default_color,
            None,
            style.strike,
            false,
        )
    };
    TextFormat {
        font_id,
        color,
        background: bg.unwrap_or(Color32::TRANSPARENT),
        strikethrough: if strike {
            Stroke::new(1.2_f32, color)
        } else {
            Stroke::NONE
        },
        underline: if underline {
            Stroke::new(1.0_f32, color)
        } else {
            Stroke::NONE
        },
        line_height: Some(size * line_height),
        ..Default::default()
    }
}

fn build_job(
    ctx: &RenderCtx<'_>,
    leaves: &[&Leaf],
    ts: TextStyle,
    wrap: f32,
) -> (egui::text::LayoutJob, Vec<(Range<usize>, Range<usize>)>, String) {
    let theme = ctx.theme;
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap.max(32.0);
    let mut map: Vec<(Range<usize>, Range<usize>)> = Vec::new();
    let mut text = String::new();

    for l in leaves {
        match l {
            Leaf::Span {
                text: t,
                style,
                link,
                src,
            } => {
                let start = text.len();
                text.push_str(t);
                let end = text.len();
                map.push((start..end, src.clone()));
                job.sections.push(egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: start..end,
                    format: span_format(
                        theme,
                        ts.size,
                        ts.line_height,
                        ts.bold,
                        ts.italic,
                        *style,
                        link.as_deref(),
                        ts.color,
                    ),
                });
            }
            Leaf::Break { hard, .. } => {
                text.push_str(if *hard { "\n" } else { " " });
            }
            Leaf::Footnote { name, src } => {
                let start = text.len();
                text.push_str(name);
                let end = text.len();
                map.push((start..end, src.clone()));
                job.sections.push(egui::text::LayoutSection {
                    leading_space: 0.0,
                    byte_range: start..end,
                    format: TextFormat {
                        font_id: FontId::new(ts.size * 0.72, fonts::family_for(false, false)),
                        color: theme.accent,
                        valign: Align::Center,
                        line_height: Some(ts.size * ts.line_height),
                        ..Default::default()
                    },
                });
            }
            _ => {}
        }
    }
    job.text = text.clone();
    (job, map, text)
}

/// Draw a run of inline content, returning the hit regions.
fn inline_flow(
    ui: &mut Ui,
    ctx: &RenderCtx<'_>,
    ts: TextStyle,
    leaves: &[Leaf],
    avail: f32,
) -> Vec<Hit> {
    let chunks = split_chunks(leaves);
    let mut hits: Vec<Hit> = Vec::new();
    let mut row_has_content = false;

    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.set_max_width(avail);
        for ch in &chunks {
            match ch {
                Chunk::Text(leaves) => {
                    if leaves.is_empty() {
                        continue;
                    }
                    let mut w = ui.available_width();
                    if row_has_content && w < avail * 0.42 {
                        ui.end_row();
                        row_has_content = false;
                        w = ui.available_width();
                    }
                    let (job, map, text) = build_job(ctx, leaves, ts, w);
                    if text.is_empty() {
                        continue;
                    }
                    let galley = ui.fonts(|f| f.layout_job(job));
                    if galley.rows.len() > 1 && row_has_content && w < avail * 0.55 {
                        ui.end_row();
                        let (job2, map2, text2) =
                            build_job(ctx, leaves, ts, ui.available_width());
                        let g2 = ui.fonts(|f| f.layout_job(job2));
                        let r = ui.add(
                            egui::Label::new(g2.clone()).sense(Sense::click()),
                        );
                        hits.push(Hit {
                            rect: r.rect,
                            galley: g2,
                            text: text2,
                            map: map2,
                            kind: HitKind::Text,
                        });
                    } else {
                        let r = ui.add(
                            egui::Label::new(galley.clone()).sense(Sense::click()),
                        );
                        hits.push(Hit {
                            rect: r.rect,
                            galley,
                            text,
                            map,
                            kind: HitKind::Text,
                        });
                    }
                    row_has_content = true;
                }
                Chunk::Media(leaf) => match leaf {
                    Leaf::Image { url, alt, src } => {
                        let uri = resolve_image(ctx.doc_dir.as_deref(), url);
                        let max_w = (avail * 0.94).max(40.0);
                        let mut image = egui::Image::from_uri(uri).maintain_aspect_ratio(true);
                        if let Some((w, h)) = image_dimensions(ctx.doc_dir.as_deref(), url) {
                            let scale = (1.0f32).min(max_w / w.max(1.0));
                            image = image.fit_to_exact_size(vec2(w * scale, h * scale));
                        } else {
                            image = image.max_width(max_w);
                        }
                        let r = ui.add(image.sense(Sense::click()));
                        hits.push(Hit {
                            rect: r.rect,
                            galley: ui.fonts(|f| {
                                f.layout_no_wrap(
                                    alt.clone(),
                                    FontId::proportional(ts.size),
                                    ts.color,
                                )
                            }),
                            text: alt.clone(),
                            map: vec![(0..alt.len(), src.clone())],
                            kind: HitKind::Atom,
                        });
                        row_has_content = true;
                    }
                    Leaf::Math { tex, display, src } => {
                        let size = if *display {
                            ts.size * 1.15
                        } else {
                            ts.size * 1.02
                        };
                        let mb = math::layout(ui, tex, size, ts.color, *display);
                        let sz = vec2(mb.width.max(4.0), mb.height().max(size));
                        let (rect, _) = ui.allocate_exact_size(sz, Sense::click());
                        math::paint(
                            ui.painter(),
                            Pos2::new(
                                rect.left(),
                                rect.top() + (rect.height() - mb.height()) * 0.5 + mb.ascent,
                            ),
                            &mb,
                            ts.color,
                        );
                        hits.push(Hit {
                            rect,
                            galley: ui.fonts(|f| {
                                f.layout_no_wrap(
                                    tex.clone(),
                                    FontId::proportional(size),
                                    ts.color,
                                )
                            }),
                            text: tex.clone(),
                            map: vec![(0..tex.len(), src.clone())],
                            kind: HitKind::Atom,
                        });
                        row_has_content = true;
                    }
                    Leaf::Footnote { name, .. } => {
                        let g = ui.fonts(|f| {
                            f.layout_no_wrap(
                                format!("[{name}]"),
                                FontId::proportional(ts.size * 0.72),
                                ctx.theme.accent,
                            )
                        });
                        ui.add(egui::Label::new(g));
                        row_has_content = true;
                    }
                    _ => {}
                },
            }
        }
    });
    hits
}

// ===========================================================================
// Images
// ===========================================================================

fn image_cache() -> &'static Mutex<HashMap<PathBuf, Option<(f32, f32)>>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, Option<(f32, f32)>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn local_image_path(doc_dir: Option<&Path>, url: &str) -> Option<PathBuf> {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:") {
        return None;
    }
    let p = PathBuf::from(url);
    if p.is_absolute() {
        Some(p)
    } else {
        doc_dir.map(|d| d.join(p))
    }
}

/// Pixel size of a local image, cached so we do not re-read headers per frame.
pub(crate) fn image_dimensions(doc_dir: Option<&Path>, url: &str) -> Option<(f32, f32)> {
    let path = local_image_path(doc_dir, url)?;
    if let Ok(c) = image_cache().lock() {
        if let Some(hit) = c.get(&path) {
            return *hit;
        }
    }
    let dims = read_image_dims(&path);
    if let Ok(mut c) = image_cache().lock() {
        if c.len() > 256 {
            c.clear();
        }
        c.insert(path, dims);
    }
    dims
}

fn read_image_dims(path: &Path) -> Option<(f32, f32)> {
    let reader = ::image::ImageReader::open(path).ok()?;
    let (w, h) = reader.into_dimensions().ok()?;
    Some((w as f32, h as f32))
}

/// Turn a Markdown image URL into something egui's loader understands.
pub(crate) fn resolve_image(doc_dir: Option<&Path>, url: &str) -> String {
    if url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("data:")
        || url.starts_with("file://")
    {
        return url.to_string();
    }
    let p = PathBuf::from(url);
    let abs = if p.is_absolute() {
        p
    } else if let Some(dir) = doc_dir {
        dir.join(p)
    } else {
        p
    };
    let mut s = String::from("file://");
    s.push_str(&abs.to_string_lossy());
    s
}

// ===========================================================================
// Block rendering
// ===========================================================================

pub fn render_block(ui: &mut Ui, ctx: &RenderCtx<'_>, idx: usize) -> Rendered {
    let block = &ctx.parsed.blocks[idx];
    let mut out = Rendered::default();
    let lead = ctx.indent_of(block);
    let outer = ui.available_width();
    let width = (outer - lead).max(80.0);

    let dim = ctx.focus_mode && ctx.active != Some(idx);
    let color = if dim { ctx.theme.focus_dim } else { ctx.theme.text };

    let row = ui.horizontal_top(|ui| {
        if lead > 0.0 {
            ui.add_space(lead);
        }
        ui.vertical(|ui| {
            ui.set_max_width(width);
            render_inner(ui, ctx, idx, &mut out, color);
        })
        .response
        .rect
    });

    out.content_rect = row.inner;
    out.rect = Rect::from_min_size(
        Pos2::new(out.content_rect.left() - lead, out.content_rect.top()),
        vec2(outer, out.content_rect.height()),
    );

    let painter = ui.painter();
    let theme = ctx.theme;

    for q in 0..block.quote {
        let x = out.content_rect.left() - lead + q as f32 * 18.0 - 9.0;
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x, out.content_rect.top() - 1.0),
                Pos2::new(x + 3.0, out.content_rect.bottom() + 1.0),
            ),
            1.5,
            theme.quote_bar,
        );
    }

    if let BlockKind::Heading { level } = &block.kind {
        if *level <= 2 {
            let y = out.content_rect.bottom() + 5.0;
            painter.line_segment(
                [
                    Pos2::new(out.content_rect.left(), y),
                    Pos2::new(out.content_rect.right(), y),
                ],
                Stroke::new(1.0_f32, theme.border),
            );
        }
    }

    if matches!(block.kind, BlockKind::Rule) {
        let y = out.content_rect.center().y;
        painter.line_segment(
            [
                Pos2::new(out.content_rect.left(), y),
                Pos2::new(out.content_rect.right(), y),
            ],
            Stroke::new(1.6_f32, theme.border_strong),
        );
    }

    out
}

fn inline_leaves(ctx: &RenderCtx<'_>, block: &Block) -> Vec<Leaf> {
    parser::parse_inline(
        &block.content,
        &parser::InlineCtx {
            defs: &ctx.parsed.defs,
            keep_markers: false,
        },
    )
}

fn render_inner(ui: &mut Ui, ctx: &RenderCtx<'_>, idx: usize, out: &mut Rendered, color: Color32) {
    let block = &ctx.parsed.blocks[idx];
    let theme = ctx.theme;
    let ts = TextStyle {
        size: ctx.base,
        color,
        bold: false,
        italic: false,
        line_height: ctx.line_height,
    };

    match &block.kind {
        BlockKind::Blank => {
            let h = ctx.base * ctx.line_height * 0.8;
            ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
        }

        BlockKind::Heading { level } => {
            let size = ctx.heading_size(*level);
            let ts = TextStyle {
                size,
                color: if matches!(level, 5 | 6) {
                    theme.text_muted
                } else {
                    theme.heading
                },
                bold: true,
                italic: false,
                line_height: 1.34,
            };
            let leaves = inline_leaves(ctx, block);
            let avail = ui.available_width();
            out.hits.extend(inline_flow(ui, ctx, ts, &leaves, avail));
        }

        BlockKind::Paragraph => {
            let leaves = inline_leaves(ctx, block);
            let avail = ui.available_width();
            out.hits.extend(inline_flow(ui, ctx, ts, &leaves, avail));
        }

        BlockKind::ListItem(li) => render_list_item(ui, ctx, idx, block, li, out, color),

        BlockKind::Code(cb) => render_code(ui, ctx, idx, block, cb),

        BlockKind::Math { .. } => {
            let size = ctx.base * 1.12;
            let mb = math::layout(ui, &block.content, size, color, true);
            let h = mb.height() + ctx.base * 0.7;
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
            math::paint(
                ui.painter(),
                Pos2::new(
                    rect.center().x - mb.width * 0.5,
                    rect.center().y + mb.ascent - mb.height() * 0.5,
                ),
                &mb,
                color,
            );
        }

        BlockKind::Table {
            aligns,
            rows,
            header_rows,
        } => render_table(ui, ctx, block, aligns, rows, *header_rows, color, out),

        BlockKind::Html => {
            let hctx = crate::html::Ctx {
                theme,
                defs: &ctx.parsed.defs,
                doc_dir: ctx.doc_dir.as_deref(),
                size: ctx.base,
                line_height: ctx.line_height,
                color,
            };
            // `None` means the fragment has no structure we understand, which
            // is the signal to fall back to rendering it as a paragraph — where
            // the inline HTML handling still applies.
            if crate::html::draw(ui, &hctx, &block.content).is_none() {
                let leaves = inline_leaves(ctx, block);
                let avail = ui.available_width();
                out.hits.extend(inline_flow(ui, ctx, ts, &leaves, avail));
            }
        }

        BlockKind::Rule => {
            ui.allocate_exact_size(vec2(ui.available_width(), ctx.base * 0.5), Sense::hover());
        }

        BlockKind::LinkDef { label, .. } => {
            let g = ui.fonts(|f| {
                f.layout_no_wrap(
                    format!("[{label}]: \u{2026}"),
                    FontId::new(ctx.base * 0.84, fonts::mono_family_for(false)),
                    theme.text_faint,
                )
            });
            ui.add(egui::Label::new(g));
        }

        BlockKind::FootnoteDef { .. } => {
            let leaves = inline_leaves(ctx, block);
            let ts = TextStyle {
                size: ctx.base * 0.9,
                color: theme.text_muted,
                bold: false,
                italic: false,
                line_height: ctx.line_height,
            };
            let avail = ui.available_width();
            out.hits.extend(inline_flow(ui, ctx, ts, &leaves, avail));
        }
    }
}

fn render_list_item(
    ui: &mut Ui,
    ctx: &RenderCtx<'_>,
    idx: usize,
    block: &Block,
    li: &ListItem,
    out: &mut Rendered,
    color: Color32,
) {
    let theme = ctx.theme;
    let line_h = ctx.base * ctx.line_height;
    let gutter = 24.0;
    let mut marker_clicked = false;

    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let (grect, gresp) = ui.allocate_exact_size(vec2(gutter, line_h), Sense::click());
        let cy = grect.top() + line_h * 0.5;
        match li.task {
            Some(done) => {
                let p = ui.painter();
                let r = Rect::from_center_size(Pos2::new(grect.left() + 8.0, cy), vec2(14.0, 14.0));
                p.rect(
                    r,
                    3.0,
                    if done { theme.accent } else { Color32::TRANSPARENT },
                    Stroke::new(
                        1.4_f32,
                        if done {
                            theme.accent
                        } else {
                            theme.border_strong
                        },
                    ),
                    egui::StrokeKind::Middle,
                );
                if done {
                    let c = r.center();
                    let s = Stroke::new(2.0_f32, theme.bg);
                    p.line_segment(
                        [
                            Pos2::new(c.x - 3.4, c.y),
                            Pos2::new(c.x - 0.8, c.y + 2.7),
                        ],
                        s,
                    );
                    p.line_segment(
                        [
                            Pos2::new(c.x - 0.8, c.y + 2.7),
                            Pos2::new(c.x + 3.6, c.y - 2.8),
                        ],
                        s,
                    );
                }
                if gresp.clicked() {
                    marker_clicked = true;
                }
            }
            None => {
                if li.ordered {
                    let g = ui.fonts(|f| {
                        f.layout_no_wrap(
                            format!("{}{}", li.number, li.delim),
                            FontId::new(ctx.base * 0.94, fonts::family_for(false, false)),
                            theme.text_muted,
                        )
                    });
                    let pos = Pos2::new(
                        grect.right() - 6.0 - g.rect.width(),
                        grect.top() + (line_h - g.rect.height()) * 0.5,
                    );
                    ui.painter().galley(pos, g, theme.text_muted);
                } else {
                    let p = ui.painter();
                    let c = Pos2::new(grect.left() + 7.0, cy);
                    let _ = match li.depth % 3 {
                        0 => p.circle_filled(c, 2.6, theme.text_muted),
                        1 => p.rect_stroke(
                            Rect::from_center_size(c, vec2(5.0, 5.0)),
                            0.5,
                            Stroke::new(1.6_f32, theme.text_muted),
                            egui::StrokeKind::Middle,
                        ),
                        _ => p.add(Shape::convex_polygon(
                            vec![
                                Pos2::new(c.x, c.y - 3.2),
                                Pos2::new(c.x + 3.0, c.y + 2.0),
                                Pos2::new(c.x - 3.0, c.y + 2.0),
                            ],
                            theme.text_muted,
                            Stroke::NONE,
                        )),
                    };
                }
            }
        }

        let leaves = inline_leaves(ctx, block);
        let ts = TextStyle {
            size: ctx.base,
            color,
            bold: false,
            italic: false,
            line_height: ctx.line_height,
        };
        let avail = ui.available_width();
        out.hits.extend(inline_flow(ui, ctx, ts, &leaves, avail));
    });

    if marker_clicked {
        out.toggled_task = Some(idx);
    }
}

fn render_code(ui: &mut Ui, ctx: &RenderCtx<'_>, idx: usize, block: &Block, cb: &parser::CodeBlock) {
    let theme = ctx.theme;
    let size = ctx.base * 0.88;

    // A Mermaid fence becomes a real diagram. The source is still one click
    // away: activating the block hands it back to the editor as text, which is
    // what makes a mis-drawn diagram debuggable instead of mystifying.
    if crate::mermaid::is_mermaid(&cb.lang)
        && crate::mermaid::draw(ui, &block.content, theme, ctx.base).is_some()
    {
        return;
    }

    let line_h = size * 1.62;
    let font = FontId::new(size, fonts::mono_family_for(false));
    let pad = 12.0;

    let lines = code_hl::highlight(&block.content, &cb.lang, theme.code_theme(), theme.code_text);
    let avail = ui.available_width();
    let wrap = ctx.wrap_code;
    let text_w = (avail - pad * 2.0).max(40.0);
    let bg_idx = ui.painter().add(Shape::Noop);

    let paint_lines = |ui: &Ui, origin: Pos2| {
        let painter = ui.painter();
        let mut y = origin.y;
        for line in &lines {
            let mut x = origin.x;
            let mut row_h = line_h;
            for tok in line {
                let w = if wrap {
                    (text_w - (x - origin.x)).max(24.0)
                } else {
                    f32::INFINITY
                };
                let job = egui::text::LayoutJob {
                    text: tok.text.clone(),
                    sections: vec![egui::text::LayoutSection {
                        leading_space: 0.0,
                        byte_range: 0..tok.text.len(),
                        format: TextFormat {
                            font_id: font.clone(),
                            color: tok.color,
                            line_height: Some(line_h),
                            ..Default::default()
                        },
                    }],
                    wrap: egui::text::TextWrapping {
                        max_width: w,
                        max_rows: usize::MAX,
                        break_anywhere: false,
                        overflow_character: None,
                    },
                    ..Default::default()
                };
                let g = ui.fonts(|f| f.layout_job(job));
                painter.galley(Pos2::new(x, y), g.clone(), tok.color);
                x += g.rect.width();
                if wrap {
                    row_h = row_h.max(g.rect.height());
                }
            }
            y += row_h;
        }
    };

    let outer: Rect = if wrap {
        let content_h = lines.len().max(1) as f32 * line_h + pad * 2.0;
        let (rect, _) = ui.allocate_exact_size(vec2(avail, content_h), Sense::click());
        ui.painter().set(bg_idx, Shape::rect_filled(rect, 7.0, theme.code_bg));
        paint_lines(ui, Pos2::new(rect.left() + pad, rect.top() + pad));
        rect
    } else {
        let char_w = ui.fonts(|f| f.glyph_width(&font, ' '));
        let max_chars = code_hl::max_line_chars(&block.content);
        let content_w = (max_chars as f32 * char_w + pad * 2.0 + 8.0).max(avail);
        let viewport_h = lines.len().max(1) as f32 * line_h + pad * 2.0;
        let inner = egui::ScrollArea::horizontal()
            .id_salt(("code", idx, block.range.start))
            .max_height(viewport_h)
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(content_w, viewport_h), Sense::click());
                paint_lines(ui, Pos2::new(rect.left() + pad, rect.top() + pad));
            });
        let rect = Rect::from_min_size(inner.inner_rect.min, vec2(avail, viewport_h));
        ui.painter().set(bg_idx, Shape::rect_filled(rect, 7.0, theme.code_bg));
        rect
    };

    if !cb.lang.is_empty() {
        let accent = code_hl::lang_accent(&cb.lang, theme.mode.is_dark());
        let g = ui.fonts(|f| {
            f.layout_no_wrap(
                cb.lang.clone(),
                FontId::new(size * 0.82, fonts::mono_family_for(false)),
                accent,
            )
        });
        let pos = Pos2::new(
            outer.right() - pad - g.rect.width(),
            outer.top() + pad * 0.55,
        );
        ui.painter().galley(pos, g, accent);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_table(
    ui: &mut Ui,
    ctx: &RenderCtx<'_>,
    block: &Block,
    aligns: &[ColAlign],
    rows: &[Vec<Range<usize>>],
    header_rows: usize,
    color: Color32,
    out: &mut Rendered,
) {
    let theme = ctx.theme;
    let size = ctx.base * 0.94;
    let line_h = size * ctx.line_height;
    let pad_x = 10.0;
    let pad_y = 7.0;
    let ncol = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    let avail = ui.available_width();

    if rows.is_empty() {
        ui.allocate_exact_size(vec2(avail, line_h), Sense::hover());
        return;
    }

    let mut col_w = vec![0.0f32; ncol];
    for row in rows {
        for (c, cell) in row.iter().enumerate() {
            let text = cell_text(block, cell);
            let g = ui.fonts(|f| {
                f.layout_no_wrap(
                    text,
                    FontId::new(size, fonts::family_for(false, false)),
                    color,
                )
            });
            col_w[c] = col_w[c].max(g.rect.width() + 4.0);
        }
    }
    let total: f32 = col_w.iter().sum::<f32>() + pad_x * 2.0 * ncol as f32;
    if total > avail {
        let k = (avail / total).max(0.3);
        for w in col_w.iter_mut() {
            *w *= k;
        }
    }
    for w in col_w.iter_mut() {
        *w = w.max(56.0);
    }

    let left = ui.cursor().left();
    let top = ui.cursor().top();
    let mut y = top;
    let mut layout_rows: Vec<(f32, f32, Vec<(f32, f32, Arc<Galley>, String, Range<usize>)>)> =
        Vec::new();

    for row in rows.iter() {
        let mut x = left;
        let mut row_h = line_h + pad_y * 2.0;
        let mut cells = Vec::new();
        for (c, cell) in row.iter().enumerate() {
            let cw = *col_w.get(c).unwrap_or(&56.0);
            let text = cell_text(block, cell);
            let galley = ui.fonts(|f| {
                f.layout(
                    text.clone(),
                    FontId::new(size, fonts::family_for(false, false)),
                    color,
                    cw,
                )
            });
            row_h = row_h.max(galley.rect.height() + pad_y * 2.0);
            cells.push((x, cw, galley, text, cell.clone()));
            x += cw + pad_x * 2.0;
        }
        layout_rows.push((y, row_h, cells));
        y += row_h;
    }

    let total_w = (col_w.iter().sum::<f32>() + pad_x * 2.0 * ncol as f32).max(avail);
    let outer = Rect::from_min_size(Pos2::new(left, top), vec2(total_w, y - top));

    let bg_idx = ui.painter().add(Shape::Noop);
    let head_idx = ui.painter().add(Shape::Noop);

    ui.allocate_exact_size(vec2(avail, y - top), Sense::hover());

    ui.painter().set(
        bg_idx,
        Shape::rect_stroke(
            outer,
            5.0,
            Stroke::new(1.0_f32, theme.border),
            egui::StrokeKind::Inside,
        ),
    );

    if header_rows > 0 {
        if let Some((hy, hh, _)) = layout_rows.first() {
            ui.painter().set(
                head_idx,
                Shape::rect_filled(
                    Rect::from_min_size(Pos2::new(outer.left(), *hy), vec2(total_w, *hh)),
                    0.0,
                    theme.table_head_bg,
                ),
            );
        }
    }

    for (r, (ry, rh, cells)) in layout_rows.iter().enumerate() {
        if r > 0 {
            ui.painter().line_segment(
                [
                    Pos2::new(outer.left(), *ry),
                    Pos2::new(outer.left() + total_w, *ry),
                ],
                Stroke::new(1.0_f32, theme.border),
            );
        }
        for (i, (cx, cw, galley, text, src)) in cells.iter().enumerate() {
            let align = aligns.get(i).copied().unwrap_or(ColAlign::None);
            let used = galley.rect.width();
            let tx = match align {
                ColAlign::Center => cx + pad_x + (cw - used) * 0.5,
                ColAlign::Right => cx + pad_x + cw - used,
                _ => cx + pad_x,
            };
            let pos = Pos2::new(tx, ry + (rh - galley.rect.height()) * 0.5);
            ui.painter().galley(pos, galley.clone(), color);
            out.hits.push(Hit {
                rect: Rect::from_min_size(pos, galley.rect.size()),
                galley: galley.clone(),
                text: text.clone(),
                map: vec![(0..text.len(), src.clone())],
                kind: HitKind::Text,
            });
        }
    }
}

/// Cell text with inline markup stripped, for measurement.
fn cell_text(block: &Block, cell: &Range<usize>) -> String {
    let s = block.content.get(cell.clone()).unwrap_or("").trim();
    let leaves = parser::parse_inline(s, &parser::InlineCtx::default());
    let mut buf = String::new();
    let mut tmp = String::new();
    for l in &leaves {
        buf.push_str(l.plain(&mut tmp));
    }
    if buf.trim().is_empty() {
        s.to_string()
    } else {
        buf
    }
}
