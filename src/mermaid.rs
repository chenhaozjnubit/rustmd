//! Mermaid diagrams, drawn straight into egui.
//!
//! There is no JavaScript engine here and none is coming: a `.md` file has to
//! render in the same process that reads it, offline. So each diagram type is
//! parsed into a small intermediate form, laid out with plain arithmetic, and
//! painted with egui shapes — the same approach `math.rs` takes for LaTeX.
//!
//! Layout is deliberately separated from painting. `layout()` needs a `Ui` only
//! to measure text, so every diagram type can be laid out in a headless test;
//! `paint()` is a dumb translation of the finished `Scene` into shapes.
//!
//! Anything that cannot be parsed returns `None`, and the caller falls back to
//! rendering the fence as an ordinary code block. That matters: a diagram this
//! version does not understand should still be *readable as source*, never
//! silently swallowed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use egui::epaint::{CubicBezierShape, Galley, Shape};
use egui::{pos2, vec2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::fonts;
use crate::theme::Theme;

// ===========================================================================
// Scene — the intermediate form every diagram type produces
// ===========================================================================

#[derive(Clone)]
enum Item {
    Rect {
        r: Rect,
        radius: f32,
        fill: Color32,
        stroke: Stroke,
    },
    Poly {
        pts: Vec<Pos2>,
        fill: Color32,
        stroke: Stroke,
    },
    Curve {
        a: Pos2,
        c1: Pos2,
        c2: Pos2,
        b: Pos2,
        stroke: Stroke,
        dashed: bool,
    },
    Line {
        a: Pos2,
        b: Pos2,
        stroke: Stroke,
        dashed: bool,
    },
    Dot {
        c: Pos2,
        r: f32,
        fill: Color32,
        stroke: Stroke,
    },
    /// A pie slice. Angles in radians, 0 = 3 o'clock, growing clockwise.
    Wedge {
        c: Pos2,
        r: f32,
        a0: f32,
        a1: f32,
        fill: Color32,
    },
    Text {
        pos: Pos2,
        galley: Arc<Galley>,
        color: Color32,
    },
}

/// A laid-out diagram, in its own coordinate space with the origin at (0, 0).
#[derive(Clone)]
struct Scene {
    size: Vec2,
    items: Vec<Item>,
}

impl Scene {
    fn new() -> Self {
        Self {
            size: Vec2::ZERO,
            items: Vec::new(),
        }
    }

    /// Every text item, in draw order. Used by the tests to assert that labels
    /// survive parsing and layout.
    #[cfg(test)]
    pub(crate) fn texts(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Text { galley, .. } => Some(galley.job.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn count(&self, pick: fn(&Item) -> bool) -> usize {
        self.items.iter().filter(|i| pick(i)).count()
    }
}

// ===========================================================================
// Painting
// ===========================================================================

/// `Shape::dashed_line` takes absolute pixel lengths, so these have to read as
/// a dashed line at diagram scale.
const DASH: f32 = 5.0;
const GAP: f32 = 4.0;

fn paint(ui: &Ui, origin: Pos2, scene: &Scene) {
    let p = ui.painter();
    let at = |q: Pos2| origin + q.to_vec2();
    for it in &scene.items {
        match it {
            Item::Rect {
                r,
                radius,
                fill,
                stroke,
            } => {
                let rr = Rect::from_min_max(at(r.min), at(r.max));
                if fill.a() > 0 {
                    p.add(Shape::rect_filled(rr, *radius, *fill));
                }
                if stroke.width > 0.0 {
                    p.add(Shape::rect_stroke(rr, *radius, *stroke, egui::StrokeKind::Middle));
                }
            }
            Item::Poly { pts, fill, stroke } => {
                let v: Vec<Pos2> = pts.iter().map(|q| at(*q)).collect();
                if v.len() >= 3 {
                    p.add(Shape::convex_polygon(v, *fill, *stroke));
                }
            }
            Item::Curve {
                a,
                c1,
                c2,
                b,
                stroke,
                dashed,
            } => {
                if *dashed {
                    let pts: Vec<Pos2> = (0..=28)
                        .map(|k| at(bez(*a, *c1, *c2, *b, k as f32 / 28.0)))
                        .collect();
                    for s in Shape::dashed_line(&pts, *stroke, DASH, GAP) {
                        p.add(s);
                    }
                } else {
                    p.add(Shape::CubicBezier(CubicBezierShape::from_points_stroke(
                        [at(*a), at(*c1), at(*c2), at(*b)],
                        false,
                        Color32::TRANSPARENT,
                        *stroke,
                    )));
                }
            }
            Item::Line {
                a,
                b,
                stroke,
                dashed,
            } => {
                if *dashed {
                    for s in Shape::dashed_line(&[at(*a), at(*b)], *stroke, DASH, GAP) {
                        p.add(s);
                    }
                } else {
                    p.line_segment([at(*a), at(*b)], *stroke);
                }
            }
            Item::Dot { c, r, fill, stroke } => {
                if stroke.width > 0.0 {
                    p.add(Shape::circle_stroke(at(*c), *r, *stroke));
                }
                if fill.a() > 0 {
                    p.add(Shape::circle_filled(at(*c), *r, *fill));
                }
            }
            Item::Wedge { c, r, a0, a1, fill } => {
                // A slice wider than a straight angle is not convex, so it goes
                // out as a fan of pieces that each are.
                let span = a1 - a0;
                let steps = (span.abs() / (std::f32::consts::PI / 2.0)).ceil().max(1.0) as usize;
                for k in 0..steps {
                    let t0 = a0 + span * (k as f32 / steps as f32);
                    let t1 = a0 + span * ((k + 1) as f32 / steps as f32);
                    let mut pts = vec![at(*c)];
                    let n = 14;
                    for j in 0..=n {
                        let t = t0 + (t1 - t0) * (j as f32 / n as f32);
                        pts.push(at(pos2(c.x + r * t.cos(), c.y + r * t.sin())));
                    }
                    p.add(Shape::convex_polygon(pts, *fill, Stroke::NONE));
                }
            }
            Item::Text { pos, galley, color } => {
                p.galley(at(*pos), galley.clone(), *color);
            }
        }
    }
}

fn bez(a: Pos2, c1: Pos2, c2: Pos2, b: Pos2, t: f32) -> Pos2 {
    let u = 1.0 - t;
    let (u2, t2) = (u * u, t * t);
    pos2(
        u2 * u * a.x + 3.0 * u2 * t * c1.x + 3.0 * u * t2 * c2.x + t2 * t * b.x,
        u2 * u * a.y + 3.0 * u2 * t * c1.y + 3.0 * u * t2 * c2.y + t2 * t * b.y,
    )
}

// ===========================================================================
// Palette
// ===========================================================================

struct Palette {
    node_fill: Color32,
    node_stroke: Color32,
    node_text: Color32,
    line: Color32,
    line_text: Color32,
    band: Color32,
    slices: [Color32; 8],
}

const SLICES: [Color32; 8] = [
    Color32::from_rgb(0x4E, 0x79, 0xA7),
    Color32::from_rgb(0xF2, 0x8E, 0x2B),
    Color32::from_rgb(0x59, 0xA1, 0x4F),
    Color32::from_rgb(0xE1, 0x57, 0x59),
    Color32::from_rgb(0xB0, 0x7A, 0xA1),
    Color32::from_rgb(0x76, 0xB7, 0xB2),
    Color32::from_rgb(0xED, 0xC9, 0x48),
    Color32::from_rgb(0x9C, 0x75, 0x5F),
];

impl Palette {
    fn new(theme: &Theme) -> Self {
        if theme.mode.is_dark() {
            Self {
                node_fill: Color32::from_rgb(0x2A, 0x2F, 0x3B),
                node_stroke: Color32::from_rgb(0x6E, 0x7C, 0xA8),
                node_text: theme.text,
                line: Color32::from_rgb(0x8B, 0x94, 0xA1),
                line_text: theme.text_muted,
                band: Color32::from_rgb(0x33, 0x3A, 0x48),
                slices: [
                    Color32::from_rgb(0x6C, 0x9E, 0xD8),
                    Color32::from_rgb(0xE8, 0xA9, 0x5C),
                    Color32::from_rgb(0x7C, 0xC4, 0x77),
                    Color32::from_rgb(0xE0, 0x7B, 0x7D),
                    Color32::from_rgb(0xC0, 0x96, 0xC8),
                    Color32::from_rgb(0x8C, 0xC9, 0xC4),
                    Color32::from_rgb(0xE5, 0xC9, 0x6B),
                    Color32::from_rgb(0xB5, 0x93, 0x7F),
                ],
            }
        } else {
            Self {
                node_fill: Color32::from_rgb(0xEC, 0xEC, 0xFF),
                node_stroke: Color32::from_rgb(0x93, 0x70, 0xDB),
                node_text: Color32::from_rgb(0x33, 0x33, 0x33),
                line: Color32::from_rgb(0x54, 0x54, 0x54),
                line_text: Color32::from_rgb(0x5A, 0x64, 0x72),
                band: Color32::from_rgb(0xDC, 0xDC, 0xF6),
                slices: SLICES,
            }
        }
    }
}

// ===========================================================================
// Canvas — a drawing surface that also measures text
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Left,
    Center,
}

struct Canvas<'a> {
    ui: &'a Ui,
    pal: Palette,
    items: Vec<Item>,
    /// Edge stroke width, scaled off the base font size.
    w: f32,
}

impl<'a> Canvas<'a> {
    fn new(ui: &'a Ui, theme: &Theme, base: f32) -> Self {
        Self {
            ui,
            pal: Palette::new(theme),
            items: Vec::new(),
            w: (base * 0.075).clamp(1.0, 2.0),
        }
    }

    fn font(&self, size: f32, bold: bool, mono: bool) -> FontId {
        if mono {
            FontId::new(size, fonts::mono_family_for(bold))
        } else {
            FontId::new(size, fonts::family_for(bold, false))
        }
    }

    fn galley(&self, s: &str, size: f32, color: Color32, bold: bool, mono: bool) -> Arc<Galley> {
        let f = self.font(size, bold, mono);
        self.ui
            .fonts(|fonts| fonts.layout_no_wrap(s.to_string(), f, color))
    }

    fn measure(&self, s: &str, size: f32, bold: bool, mono: bool) -> Vec2 {
        if s.is_empty() {
            return vec2(0.0, size * 1.2);
        }
        let f = self.font(size, bold, mono);
        self.ui
            .fonts(|fonts| fonts.layout_no_wrap(s.to_string(), f, Color32::BLACK))
            .rect
            .size()
    }

    /// Place one line of text. `anchor` picks which edge sits at `at.x`; the
    /// text is always vertically centred on `at.y`.
    fn text(
        &mut self,
        s: &str,
        at: Pos2,
        size: f32,
        color: Color32,
        anchor: Anchor,
        bold: bool,
        mono: bool,
    ) -> Vec2 {
        if s.is_empty() {
            return Vec2::ZERO;
        }
        let g = self.galley(s, size, color, bold, mono);
        let sz = g.rect.size();
        let x = match anchor {
            Anchor::Left => at.x,
            Anchor::Center => at.x - sz.x * 0.5,
        };
        self.items.push(Item::Text {
            pos: pos2(x, at.y - sz.y * 0.5),
            galley: g,
            color,
        });
        sz
    }

    fn rect(&mut self, r: Rect, radius: f32, fill: Color32, stroke: Stroke) {
        self.items.push(Item::Rect {
            r,
            radius,
            fill,
            stroke,
        });
    }

    fn line(&mut self, a: Pos2, b: Pos2, color: Color32, dashed: bool) {
        let w = self.w;
        self.items.push(Item::Line {
            a,
            b,
            stroke: Stroke::new(w, color),
            dashed,
        });
    }

    fn curve(&mut self, a: Pos2, c1: Pos2, c2: Pos2, b: Pos2, color: Color32, dashed: bool, w: f32) {
        self.items.push(Item::Curve {
            a,
            c1,
            c2,
            b,
            stroke: Stroke::new(w, color),
            dashed,
        });
    }

    fn poly(&mut self, pts: Vec<Pos2>, fill: Color32, stroke: Stroke) {
        self.items.push(Item::Poly { pts, fill, stroke });
    }

    fn dot(&mut self, c: Pos2, r: f32, fill: Color32, stroke: Stroke) {
        self.items.push(Item::Dot { c, r, fill, stroke });
    }

    /// An arrowhead with its tip at `tip`, pointing along `dir`.
    fn arrowhead(&mut self, tip: Pos2, dir: Vec2, head: Head, color: Color32) {
        let d = if dir.length() < 1e-4 {
            vec2(0.0, 1.0)
        } else {
            dir.normalized()
        };
        let n = vec2(-d.y, d.x);
        let w = self.w;
        match head {
            Head::None => {}
            Head::Arrow => {
                let base = tip - d * (w * 3.4);
                let hw = w * 1.65;
                self.poly(vec![tip, base + n * hw, base - n * hw], color, Stroke::NONE);
            }
            Head::Circle => {
                // The rim of the circle is the marker, so an unfilled disc.
                self.dot(tip - d * (w * 2.0), w * 2.0, Color32::TRANSPARENT, Stroke::new(w, color));
            }
            Head::Cross => {
                let c = tip - d * (w * 1.5);
                let a = n * (w * 1.9);
                let b = d * (w * 1.9);
                self.line(c + a, c - a, color, false);
                self.line(c + b, c - b, color, false);
            }
            Head::Triangle => {
                // Hollow, so it reads as "is a" rather than "points at".
                let base = tip - d * (w * 5.2);
                let hw = w * 2.6;
                self.poly(
                    vec![tip, base + n * hw, base - n * hw],
                    self.pal.node_fill,
                    Stroke::new(w, color),
                );
            }
            Head::Diamond => {
                let mid = tip - d * (w * 2.6);
                let back = tip - d * (w * 5.2);
                let hw = w * 1.9;
                self.poly(
                    vec![tip, mid + n * hw, back, mid - n * hw],
                    color,
                    Stroke::NONE,
                );
            }
        }
    }
}

// ===========================================================================
// Public entry point
// ===========================================================================

/// True when a fenced block's info string asks for a Mermaid diagram.
pub fn is_mermaid(lang: &str) -> bool {
    let l = lang.trim().to_ascii_lowercase();
    l == "mermaid" || l == "mmd"
}

type Cache = Mutex<HashMap<(u64, bool, u32), Option<Scene>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn source_hash(code: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// Lay `source` out, cached by (source, theme, base size).
///
/// Layout measures text through the font system, which is the expensive part,
/// and a diagram does not change between frames — so paying for it once per
/// edit rather than once per frame is the difference between a smooth scroll
/// and a stutter.
fn scene_for(ui: &Ui, source: &str, theme: &Theme, base: f32) -> Option<Scene> {
    let key = (source_hash(source), theme.mode.is_dark(), base.to_bits());
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let built = build(ui, source, theme, base);
    if let Ok(mut c) = cache().lock() {
        if c.len() > 64 {
            c.clear();
        }
        c.insert(key, built.clone());
    }
    built
}

/// Draw a Mermaid fence and return the height it took. `None` means "not a
/// diagram I understand" — the caller should draw a code block instead.
pub fn draw(ui: &mut Ui, source: &str, theme: &Theme, base: f32) -> Option<f32> {
    let scene = scene_for(ui, source, theme, base)?;
    let avail = ui.available_width();
    let h = scene.size.y + base * 0.9;

    if scene.size.x + 12.0 <= avail {
        let (rect, _) = ui.allocate_exact_size(vec2(avail, h), Sense::click());
        let x = rect.left() + ((avail - scene.size.x) * 0.5).max(0.0);
        paint(ui, pos2(x, rect.top() + base * 0.45), &scene);
    } else {
        // Too wide to fit. It scrolls rather than shrinks, because shrinking
        // would mean rescaling already-laid-out galleys, which egui cannot do.
        let id = ui.id().with(("mermaid", source_hash(source)));
        egui::ScrollArea::horizontal()
            .id_salt(id)
            .max_height(h)
            .show(ui, |ui| {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(scene.size.x + 16.0, h), Sense::click());
                paint(ui, pos2(rect.left() + 8.0, rect.top() + base * 0.45), &scene);
            });
    }
    Some(h)
}

fn build(ui: &Ui, source: &str, theme: &Theme, base: f32) -> Option<Scene> {
    let lines: Vec<&str> = source.lines().collect();
    let head = lines
        .iter()
        .map(|l| strip_comment(l).trim().to_string())
        .find(|l| !l.is_empty())?;
    let kw = head.split_whitespace().next().unwrap_or("");

    let mut c = Canvas::new(ui, theme, base);
    let size = base * 0.92;

    match kw {
        "flowchart" | "graph" => {
            let g = parse_flow(&lines)?;
            let dir = parse_dir(&head);
            Some(flow_layout(&mut c, &g, dir, size))
        }
        "pie" => {
            let (title, data) = parse_pie(&lines)?;
            Some(pie_layout(&mut c, &title, &data, size))
        }
        "sequenceDiagram" => {
            let s = parse_sequence(&lines)?;
            Some(seq_layout(&mut c, &s, size))
        }
        "classDiagram" => {
            let m = parse_class(&lines)?;
            Some(class_layout(&mut c, &m, size))
        }
        "stateDiagram-v2" | "stateDiagram" => {
            let g = parse_state(&lines)?;
            Some(layered_layout(
                &mut c,
                flow_boxes(&g, size),
                &g.edges,
                Dir::Down,
                size,
            ))
        }
        "erDiagram" => {
            let m = parse_er(&lines)?;
            Some(er_layout(&mut c, &m, size))
        }
        "gantt" => {
            let (title, tasks) = parse_gantt(&lines)?;
            Some(gantt_layout(&mut c, &title, &tasks, size))
        }
        "mindmap" => {
            let nodes = parse_mindmap(&lines)?;
            Some(mindmap_layout(&mut c, &nodes, size))
        }
        _ => None,
    }
}

/// The plain-box view of a graph, for diagram types whose nodes carry no shape
/// syntax of their own.
fn flow_boxes(g: &Graph, size: f32) -> Vec<Box> {
    g.nodes
        .iter()
        .map(|nd| Box {
            lines: nd
                .label
                .split('\n')
                .map(|l| BoxLine::plain(l, size))
                .collect(),
            shape: nd.shape,
            band: false,
            w: 0.0,
            h: 0.0,
            center: Pos2::ZERO,
        })
        .collect()
}

fn parse_dir(head: &str) -> Dir {
    match head.split_whitespace().nth(1) {
        Some("LR") => Dir::Right,
        Some("RL") => Dir::Left,
        Some("BT") => Dir::Up,
        _ => Dir::Down,
    }
}

// ===========================================================================
// Lexing helpers shared by every diagram type
// ===========================================================================

fn strip_comment(line: &str) -> &str {
    match line.find("%%") {
        Some(i) => &line[..i],
        None => line,
    }
}

fn skip_ws(cs: &[char], i: &mut usize) {
    while matches!(cs.get(*i), Some(c) if c.is_whitespace()) {
        *i += 1;
    }
}

fn is_glyph(c: Option<&char>) -> bool {
    matches!(c, Some('-') | Some('=') | Some('.'))
}

/// Read up to, but not including, `end`. Leaves `i` on the terminator.
fn read_until(cs: &[char], i: &mut usize, end: char) -> String {
    let mut s = String::new();
    while let Some(&c) = cs.get(*i) {
        if c == end {
            break;
        }
        s.push(c);
        *i += 1;
    }
    clean_label(s.trim())
}

fn read_until_str(cs: &[char], i: &mut usize, end: &str) -> String {
    let end: Vec<char> = end.chars().collect();
    let mut s = String::new();
    while *i < cs.len() {
        if cs[*i..].starts_with(&end) {
            break;
        }
        s.push(cs[*i]);
        *i += 1;
    }
    clean_label(s.trim())
}

/// Mermaid quotes labels, escapes them with `#quot;` and breaks lines with
/// `<br/>`. Undo all three so the text can be measured and drawn.
fn clean_label(s: &str) -> String {
    let s = s.trim();
    let s = if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    };
    s.replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("<br>", "\n")
        .replace("#quot;", "\"")
        .replace("#amp;", "&")
        .replace("#lt;", "<")
        .replace("#gt;", ">")
}

// ===========================================================================
// Diagram model
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Down,
    Up,
    Right,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeShape {
    Rect,
    Round,
    Stadium,
    Circle,
    Diamond,
    Hexagon,
    Subroutine,
    Cylinder,
    Asymmetric,
    Parallelogram,
    /// Filled dot, for state diagrams' `[*]` start marker.
    Start,
    /// Ringed dot, for state diagrams' `[*]` end marker.
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EdgeStyle {
    Solid,
    Dotted,
    Thick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Head {
    None,
    Arrow,
    Circle,
    Cross,
    /// Hollow triangle — class-diagram inheritance.
    Triangle,
    /// Filled diamond — class-diagram composition.
    Diamond,
}

#[derive(Debug, Clone)]
struct Node {
    id: String,
    label: String,
    shape: NodeShape,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    style: EdgeStyle,
    /// Marker at the source end.
    head: Head,
    /// Marker at the target end.
    tail: Head,
    /// Small text just past the source end, e.g. an ER cardinality.
    head_note: String,
    /// Small text just past the target end.
    tail_note: String,
}

impl Edge {
    fn new(from: usize, to: usize, label: String, style: EdgeStyle, head: Head, tail: Head) -> Self {
        Self {
            from,
            to,
            label,
            style,
            head,
            tail,
            head_note: String::new(),
            tail_note: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Graph {
    /// Register `id` and return its index, or return the index it already has.
    /// A later mention may carry the label (`A` on one line, `A[Text]` on the
    /// next), which Mermaid allows and real documents do.
    fn node(&mut self, id: &str, shape: NodeShape, label: String) -> usize {
        if let Some(k) = self.nodes.iter().position(|n| n.id == id) {
            if !label.is_empty() {
                self.nodes[k].label = label;
                self.nodes[k].shape = shape;
            }
            return k;
        }
        self.nodes.push(Node {
            id: id.to_string(),
            label: if label.is_empty() {
                id.to_string()
            } else {
                label
            },
            shape,
        });
        self.nodes.len() - 1
    }
}

struct EdgeRead {
    style: EdgeStyle,
    head: Head,
    tail: Head,
    label: String,
}

fn parse_flow(lines: &[&str]) -> Option<Graph> {
    let mut g = Graph::default();
    let mut seen_header = false;
    for raw in lines {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if !seen_header {
            seen_header = true;
            continue; // the header, already matched by the caller
        }
        let kw = line.split_whitespace().next().unwrap_or("");
        if matches!(
            kw,
            "subgraph"
                | "end"
                | "direction"
                | "style"
                | "classDef"
                | "class"
                | "linkStyle"
                | "click"
                | "accTitle"
                | "accDescr"
        ) {
            continue;
        }
        parse_flow_line(&line, &mut g);
    }
    if g.nodes.is_empty() {
        return None;
    }
    Some(g)
}

fn parse_flow_line(line: &str, g: &mut Graph) {
    let cs: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut prev: Option<usize> = None;
    let mut pending: Option<EdgeRead> = None;

    loop {
        skip_ws(&cs, &mut i);
        if i >= cs.len() {
            break;
        }
        if prev.is_some() {
            if let Some(sty) = read_edge_op(&cs, &mut i) {
                pending = Some(sty);
                continue;
            }
        }
        if let Some(k) = read_node_ref(&cs, &mut i, g) {
            if let (Some(p), Some(sty)) = (prev, pending.take()) {
                g.edges.push(Edge::new(p, k, sty.label, sty.style, sty.head, sty.tail));
            }
            prev = Some(k);
            continue;
        }
        i += 1;
    }
}

fn read_tail(cs: &[char], i: &mut usize) -> Head {
    match cs.get(*i) {
        Some('>') => {
            *i += 1;
            Head::Arrow
        }
        Some('o') => {
            *i += 1;
            Head::Circle
        }
        Some('x') => {
            *i += 1;
            Head::Cross
        }
        _ => Head::None,
    }
}

/// Read an edge operator: `-->`, `---`, `-.->`, `==>`, `--o`, `--x`, `<-->`,
/// `-->|label|`, `-- label -->` and friends.
///
/// Order matters. The tail is looked for before the `|label|` form, because in
/// `-->|yes|` the `>` belongs to the operator and the label follows it. The
/// `-- label -->` form is tried only once neither matched, since that is the
/// one case where text may sit *inside* the operator.
fn read_edge_op(cs: &[char], i: &mut usize) -> Option<EdgeRead> {
    let start = *i;
    let mut head = Head::None;
    if let Some(&c) = cs.get(*i) {
        if (c == '<' || c == 'o' || c == 'x') && is_glyph(cs.get(*i + 1)) {
            head = match c {
                '<' => Head::Arrow,
                'o' => Head::Circle,
                _ => Head::Cross,
            };
            *i += 1;
        }
    }

    let run_start = *i;
    let (mut dots, mut eqs) = (0, 0);
    while let Some(&c) = cs.get(*i) {
        match c {
            '.' => dots += 1,
            '=' => eqs += 1,
            '-' => {}
            _ => break,
        }
        *i += 1;
    }
    if *i - run_start < 2 {
        *i = start;
        return None;
    }
    let style = if eqs > 0 {
        EdgeStyle::Thick
    } else if dots > 0 {
        EdgeStyle::Dotted
    } else {
        EdgeStyle::Solid
    };

    let tail = read_tail(cs, i);
    if tail != Head::None {
        let label = if cs.get(*i) == Some(&'|') {
            *i += 1;
            let t = read_until(cs, i, '|');
            if cs.get(*i) == Some(&'|') {
                *i += 1;
            }
            t
        } else {
            String::new()
        };
        return Some(EdgeRead {
            style,
            head,
            tail,
            label,
        });
    }

    // `-- label -->`
    let mut k = *i;
    while cs.get(k) == Some(&' ') {
        k += 1;
    }
    let mut t = String::new();
    while let Some(&c) = cs.get(k) {
        if is_glyph(Some(&c)) {
            break;
        }
        t.push(c);
        k += 1;
    }
    let t = clean_label(t.trim());
    let mut k2 = k;
    while cs.get(k2) == Some(&' ') {
        k2 += 1;
    }
    let r2 = k2;
    while is_glyph(cs.get(k2)) {
        k2 += 1;
    }
    if !t.is_empty() && k2 - r2 >= 2 {
        let mut k3 = k2;
        let tail2 = read_tail(cs, &mut k3);
        *i = k3;
        return Some(EdgeRead {
            style,
            head,
            tail: tail2,
            label: t,
        });
    }

    Some(EdgeRead {
        style,
        head,
        tail: Head::None,
        label: String::new(),
    })
}

fn read_node_ref(cs: &[char], i: &mut usize, g: &mut Graph) -> Option<usize> {
    skip_ws(cs, i);
    let start = *i;

    let mut id = String::new();
    if cs.get(*i) == Some(&'"') {
        *i += 1;
        while let Some(&c) = cs.get(*i) {
            if c == '"' {
                *i += 1;
                break;
            }
            id.push(c);
            *i += 1;
        }
    } else {
        // Mermaid ids are alphanumeric plus `_` (and we let CJK through, since
        // documents in Chinese use them). Hyphens are deliberately excluded:
        // `A-->B` would otherwise lex as one id.
        while let Some(&c) = cs.get(*i) {
            if c.is_alphanumeric() || c == '_' || !c.is_ascii() {
                id.push(c);
                *i += 1;
            } else {
                break;
            }
        }
    }
    if id.is_empty() {
        *i = start;
        return None;
    }

    let (shape, label) = read_shape(cs, i).unwrap_or((NodeShape::Rect, String::new()));
    Some(g.node(&id, shape, label))
}

fn read_shape(cs: &[char], i: &mut usize) -> Option<(NodeShape, String)> {
    let save = *i;
    match cs.get(*i)? {
        '[' => {
            *i += 1;
            match cs.get(*i) {
                Some('[') => {
                    *i += 1;
                    let t = read_until_str(cs, i, "]]");
                    *i = (*i + 2).min(cs.len());
                    Some((NodeShape::Subroutine, t))
                }
                Some('(') => {
                    *i += 1;
                    let t = read_until(cs, i, ')');
                    *i += 1;
                    if cs.get(*i) == Some(&']') {
                        *i += 1;
                    }
                    Some((NodeShape::Cylinder, t))
                }
                Some('/') | Some('\\') => {
                    let close = cs[*i];
                    *i += 1;
                    let t = read_until(cs, i, close);
                    *i += 1;
                    if cs.get(*i) == Some(&']') {
                        *i += 1;
                    }
                    Some((NodeShape::Parallelogram, t))
                }
                _ => {
                    let t = read_until(cs, i, ']');
                    *i += 1;
                    Some((NodeShape::Rect, t))
                }
            }
        }
        '(' => {
            *i += 1;
            match cs.get(*i) {
                Some('(') => {
                    *i += 1;
                    let t = read_until_str(cs, i, "))");
                    *i = (*i + 2).min(cs.len());
                    Some((NodeShape::Circle, t))
                }
                Some('[') => {
                    *i += 1;
                    let t = read_until_str(cs, i, "])");
                    *i = (*i + 2).min(cs.len());
                    Some((NodeShape::Stadium, t))
                }
                _ => {
                    let t = read_until(cs, i, ')');
                    *i += 1;
                    Some((NodeShape::Round, t))
                }
            }
        }
        '{' => {
            *i += 1;
            match cs.get(*i) {
                Some('{') => {
                    *i += 1;
                    let t = read_until_str(cs, i, "}}");
                    *i = (*i + 2).min(cs.len());
                    Some((NodeShape::Hexagon, t))
                }
                _ => {
                    let t = read_until(cs, i, '}');
                    *i += 1;
                    Some((NodeShape::Diamond, t))
                }
            }
        }
        '>' => {
            *i += 1;
            let t = read_until(cs, i, ']');
            *i += 1;
            Some((NodeShape::Asymmetric, t))
        }
        _ => {
            *i = save;
            None
        }
    }
}

// ===========================================================================
// Layered layout — shared by flowchart, state, class and ER diagrams
// ===========================================================================

struct BoxLine {
    text: String,
    size: f32,
    bold: bool,
    mono: bool,
    /// Body lines (class members, entity attributes) are left-aligned.
    left: bool,
    /// Extra space above this line.
    space_before: f32,
}

impl BoxLine {
    fn plain(text: &str, size: f32) -> Self {
        Self {
            text: text.to_string(),
            size,
            bold: false,
            mono: false,
            left: false,
            space_before: 0.0,
        }
    }
}

/// A node as the layout sees it: geometry derived from the label, not given.
struct Box {
    lines: Vec<BoxLine>,
    shape: NodeShape,
    /// A distinct title band, used by class and ER diagrams.
    band: bool,
    w: f32,
    h: f32,
    center: Pos2,
}

/// Longest-path ranking, cycle-safe.
///
/// Kahn's algorithm layers the DAG. Whatever it cannot reach sits in a cycle,
/// and those get fresh layers so the diagram still reads top-to-bottom instead
/// of collapsing onto one row.
fn rank_nodes(n: usize, edges: &[(usize, usize)]) -> Vec<usize> {
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indeg = vec![0usize; n];
    for &(a, b) in edges {
        if a == b || adj[a].contains(&b) {
            continue;
        }
        adj[a].push(b);
        indeg[b] += 1;
    }
    let mut rank = vec![0usize; n];
    let mut done = vec![false; n];
    let mut queue: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let u = queue[head];
        head += 1;
        done[u] = true;
        for k in 0..adj[u].len() {
            let v = adj[u][k];
            rank[v] = rank[v].max(rank[u] + 1);
            indeg[v] -= 1;
            if indeg[v] == 0 {
                queue.push(v);
            }
        }
    }
    let mut next = rank.iter().copied().max().map(|m| m + 1).unwrap_or(0);
    for i in 0..n {
        if !done[i] {
            rank[i] = next;
            next += 1;
        }
    }
    rank
}

/// Barycentre ordering — a few downhill/uphill sweeps remove most crossings,
/// which is all a reader needs.
fn order_layers(layers: &mut [Vec<usize>], edges: &[(usize, usize)]) {
    let n: usize = layers.iter().map(|l| l.len()).sum();
    if n == 0 {
        return;
    }
    let mut layer_of = vec![0usize; n];
    let mut idx = vec![0usize; n];
    fn sync(layers: &[Vec<usize>], layer_of: &mut [usize], idx: &mut [usize]) {
        for (li, l) in layers.iter().enumerate() {
            for (k, &v) in l.iter().enumerate() {
                layer_of[v] = li;
                idx[v] = k;
            }
        }
    }
    sync(layers, &mut layer_of, &mut idx);

    for sweep in 0..6 {
        let down = sweep % 2 == 0;
        let range: Vec<usize> = if down {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        for li in range {
            let near = if down { li - 1 } else { li + 1 };
            let mut keyed: Vec<(f32, usize, usize)> = layers[li]
                .iter()
                .map(|&v| {
                    let (mut sum, mut cnt) = (0.0f32, 0.0f32);
                    for &(a, b) in edges {
                        let other = if a == v {
                            b
                        } else if b == v {
                            a
                        } else {
                            continue;
                        };
                        if layer_of[other] == near {
                            sum += idx[other] as f32;
                            cnt += 1.0;
                        }
                    }
                    let key = if cnt > 0.0 { sum / cnt } else { idx[v] as f32 };
                    (key, v, idx[v])
                })
                .collect();
            keyed.sort_by(|a, b| {
                a.0.partial_cmp(&b.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.2.cmp(&b.2))
            });
            layers[li] = keyed.into_iter().map(|(_, v, _)| v).collect();
            sync(layers, &mut layer_of, &mut idx);
        }
    }
}

/// Measure each box from its lines.
fn size_boxes(c: &Canvas<'_>, boxes: &mut [Box], size: f32) {
    let line_h = size * 1.4;
    for b in boxes.iter_mut() {
        let pad_x = size * 0.95;
        let pad_y = size * 0.5;
        let mut w = 0.0f32;
        let mut h = 0.0f32;
        for l in &b.lines {
            let m = c.measure(&l.text, l.size, l.bold, l.mono);
            w = w.max(m.x);
            h += l.size / size * line_h + l.space_before;
        }
        w = w.max(size * 0.6);
        h = h.max(line_h);
        let mut bw = w + pad_x * 2.0;
        let mut bh = h + pad_y * 2.0;
        match b.shape {
            NodeShape::Diamond => {
                bw *= 1.45;
                bh *= 1.5;
            }
            NodeShape::Circle => {
                let d = (bw * bw + bh * bh).sqrt() * 0.74;
                bw = d.max(bh);
                bh = bw;
            }
            NodeShape::Hexagon | NodeShape::Parallelogram => bw += bh * 0.55,
            NodeShape::Stadium => bw += bh,
            NodeShape::Cylinder => bh += bh * 0.22,
            NodeShape::Start | NodeShape::End => {
                bw = bw.min(bh);
                bh = bw;
            }
            _ => {}
        }
        b.w = bw;
        b.h = bh;
    }
}

/// Assign every box a centre. `down` stacks the layers vertically; otherwise
/// they become columns. `gap_main[i]` is the space left after layer `i`, which
/// the caller widens where an edge label has to fit.
fn place_boxes(
    boxes: &mut [Box],
    layers: &[Vec<usize>],
    down: bool,
    gap_main: &[f32],
    gap_cross: f32,
) {
    let ext = |v: usize, b: &[Box], down: bool| {
        if down {
            (b[v].w, b[v].h)
        } else {
            (b[v].h, b[v].w)
        }
    };
    let layer_main: Vec<f32> = layers
        .iter()
        .map(|l| l.iter().map(|&v| ext(v, boxes, down).1).fold(0.0f32, f32::max))
        .collect();
    let layer_cross: Vec<f32> = layers
        .iter()
        .map(|l| {
            let sum: f32 = l.iter().map(|&v| ext(v, boxes, down).0).sum();
            sum + gap_cross * (l.len().saturating_sub(1)) as f32
        })
        .collect();
    let widest = layer_cross.iter().copied().fold(0.0f32, f32::max);

    let mut main = 0.0f32;
    for (li, l) in layers.iter().enumerate() {
        let mut cross = (widest - layer_cross[li]) * 0.5;
        for &v in l {
            let (bw, bh) = (boxes[v].w, boxes[v].h);
            let cm = main + layer_main[li] * 0.5;
            let cc = cross + ext(v, boxes, down).0 * 0.5;
            boxes[v].center = if down { pos2(cc, cm) } else { pos2(cm, cc) };
            cross += ext(v, boxes, down).0 + gap_cross;
            let _ = (bw, bh);
        }
        main += layer_main[li] + gap_main.get(li).copied().unwrap_or(0.0);
    }
}

/// Mirror a finished scene in place. This is how `BT` and `RL` are supported
/// without a second copy of every layout.
fn mirror(scene: &mut Scene, flip_x: bool, flip_y: bool) {
    let (w, h) = (scene.size.x, scene.size.y);
    let fx = |p: Pos2| {
        pos2(
            if flip_x { w - p.x } else { p.x },
            if flip_y { h - p.y } else { p.y },
        )
    };
    for it in &mut scene.items {
        match it {
            Item::Rect { r, .. } => {
                let a = fx(r.min);
                let b = fx(r.max);
                *r = Rect::from_min_max(
                    pos2(a.x.min(b.x), a.y.min(b.y)),
                    pos2(a.x.max(b.x), a.y.max(b.y)),
                );
            }
            Item::Poly { pts, .. } => {
                for p in pts.iter_mut() {
                    *p = fx(*p);
                }
            }
            Item::Curve { a, c1, c2, b, .. } => {
                *a = fx(*a);
                *c1 = fx(*c1);
                *c2 = fx(*c2);
                *b = fx(*b);
            }
            Item::Line { a, b, .. } => {
                *a = fx(*a);
                *b = fx(*b);
            }
            Item::Dot { c, .. } => *c = fx(*c),
            Item::Wedge { c, a0, a1, .. } => {
                *c = fx(*c);
                if flip_x {
                    let t = *a0;
                    *a0 = std::f32::consts::PI - *a1;
                    *a1 = std::f32::consts::PI - t;
                }
                if flip_y {
                    let t = *a0;
                    *a0 = -*a1;
                    *a1 = -t;
                }
            }
            Item::Text { pos, galley, .. } => {
                let sz = galley.rect.size();
                let a = fx(*pos);
                let b = fx(pos2(pos.x + sz.x, pos.y + sz.y));
                *pos = pos2(a.x.min(b.x), a.y.min(b.y));
            }
        }
    }
}

// ===========================================================================
// Shared drawing helpers for layered diagrams
// ===========================================================================

fn corner_radius(shape: NodeShape, h: f32) -> f32 {
    match shape {
        NodeShape::Round | NodeShape::Stadium | NodeShape::Cylinder => h * 0.5,
        NodeShape::Rect | NodeShape::Subroutine | NodeShape::Parallelogram => 3.0,
        _ => 4.0,
    }
}

/// Paint one measured box (fill + stroke + its lines).
fn draw_box(c: &mut Canvas<'_>, b: &Box, size: f32) {
    let line_h = size * 1.4;
    let r = Rect::from_center_size(b.center, vec2(b.w, b.h));
    match b.shape {
        NodeShape::Start => {
            c.dot(b.center, b.h * 0.34, c.pal.node_text, Stroke::NONE);
            return;
        }
        NodeShape::End => {
            c.dot(
                b.center,
                b.h * 0.34,
                Color32::TRANSPARENT,
                Stroke::new(c.w, c.pal.node_text),
            );
            c.dot(
                b.center,
                b.h * 0.34 - c.w * 2.2,
                c.pal.node_text,
                Stroke::NONE,
            );
            return;
        }
        NodeShape::Diamond => {
            c.poly(
                vec![
                    pos2(b.center.x, r.top()),
                    pos2(r.right(), b.center.y),
                    pos2(b.center.x, r.bottom()),
                    pos2(r.left(), b.center.y),
                ],
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
        }
        NodeShape::Hexagon => {
            let k = b.h * 0.5;
            c.poly(
                vec![
                    pos2(r.left() + k, r.top()),
                    pos2(r.right() - k, r.top()),
                    pos2(r.right(), b.center.y),
                    pos2(r.right() - k, r.bottom()),
                    pos2(r.left() + k, r.bottom()),
                    pos2(r.left(), b.center.y),
                ],
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
        }
        NodeShape::Circle => {
            c.dot(
                b.center,
                b.w * 0.5,
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
        }
        NodeShape::Subroutine => {
            let inset = b.w * 0.06;
            c.rect(
                r,
                3.0,
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
            c.line(
                pos2(r.left() + inset, r.top()),
                pos2(r.left() + inset, r.bottom()),
                c.pal.node_stroke,
                false,
            );
            c.line(
                pos2(r.right() - inset, r.top()),
                pos2(r.right() - inset, r.bottom()),
                c.pal.node_stroke,
                false,
            );
        }
        NodeShape::Parallelogram => {
            let k = b.h * 0.28;
            c.poly(
                vec![
                    pos2(r.left() + k, r.top()),
                    pos2(r.right(), r.top()),
                    pos2(r.right() - k, r.bottom()),
                    pos2(r.left(), r.bottom()),
                ],
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
        }
        NodeShape::Cylinder => {
            let ry = b.h * 0.11;
            c.rect(
                Rect::from_min_max(r.min, pos2(r.right(), r.bottom() - ry)),
                0.0,
                c.pal.node_fill,
                Stroke::NONE,
            );
            c.dot(
                pos2(b.center.x, r.bottom() - ry),
                b.w * 0.5,
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
            c.dot(
                pos2(b.center.x, r.top() + ry),
                b.w * 0.5,
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
            c.line(
                pos2(r.left(), r.top() + ry),
                pos2(r.left(), r.bottom() - ry),
                c.pal.node_stroke,
                false,
            );
            c.line(
                pos2(r.right(), r.top() + ry),
                pos2(r.right(), r.bottom() - ry),
                c.pal.node_stroke,
                false,
            );
        }
        NodeShape::Asymmetric => {
            let k = b.w * 0.14;
            c.poly(
                vec![
                    pos2(r.left(), r.top()),
                    pos2(r.right() - k, r.top()),
                    pos2(r.right(), b.center.y),
                    pos2(r.right() - k, r.bottom()),
                    pos2(r.left(), r.bottom()),
                ],
                c.pal.node_fill,
                Stroke::new(c.w, c.pal.node_stroke),
            );
        }
        NodeShape::Stadium | NodeShape::Round | NodeShape::Rect => {
            let radius = corner_radius(b.shape, b.h);
            c.rect(r, radius, c.pal.node_fill, Stroke::new(c.w, c.pal.node_stroke));
        }
    }

    // Lines. A title band, when present, splits the box in two.
    let mut y = r.top() + size * 0.5;
    if b.band {
        let band_h = line_h * 0.75 + size * 0.55;
        let band_rect = Rect::from_min_max(r.min, pos2(r.right(), r.top() + band_h));
        c.rect(
            band_rect,
            corner_radius(b.shape, b.h),
            c.pal.band,
            Stroke::NONE,
        );
    }
    for l in &b.lines {
        y += l.space_before;
        let anchor = if l.left { Anchor::Left } else { Anchor::Center };
        let x = if l.left { r.left() + size * 0.6 } else { b.center.x };
        c.text(&l.text, pos2(x, y + line_h * 0.5), l.size, c.pal.node_text, anchor, l.bold, l.mono);
        y += l.size / size * line_h;
    }
}

/// Normalise a canvas into a scene whose origin is (0, 0).
fn finish(c: &mut Canvas<'_>) -> Scene {
    let items = std::mem::take(&mut c.items);
    let mut min = pos2(f32::INFINITY, f32::INFINITY);
    let mut max = pos2(f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut grow = |p: Pos2| {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    };
    for it in &items {
        match it {
            Item::Rect { r, .. } => {
                grow(r.min);
                grow(r.max);
            }
            Item::Poly { pts, .. } => {
                for p in pts {
                    grow(*p);
                }
            }
            Item::Curve { a, c1, c2, b, .. } => {
                for p in [a, c1, c2, b] {
                    grow(*p);
                }
            }
            Item::Line { a, b, .. } => {
                grow(*a);
                grow(*b);
            }
            Item::Dot { c: cc, r, .. } | Item::Wedge { c: cc, r, .. } => {
                grow(pos2(cc.x - r, cc.y - r));
                grow(pos2(cc.x + r, cc.y + r));
            }
            Item::Text { pos, galley, .. } => {
                grow(*pos);
                grow(pos2(pos.x + galley.rect.width(), pos.y + galley.rect.height()));
            }
        }
    }
    if items.is_empty() || !min.x.is_finite() {
        return Scene::new();
    }
    let shift = vec2(-min.x, -min.y);
    let mut scene = Scene {
        size: max - min,
        items,
    };
    if shift != Vec2::ZERO {
        for it in &mut scene.items {
            match it {
                Item::Rect { r, .. } => *r = r.translate(shift),
                Item::Poly { pts, .. } => {
                    for p in pts.iter_mut() {
                        *p += shift;
                    }
                }
                Item::Curve { a, c1, c2, b, .. } => {
                    *a += shift;
                    *c1 += shift;
                    *c2 += shift;
                    *b += shift;
                }
                Item::Line { a, b, .. } => {
                    *a += shift;
                    *b += shift;
                }
                Item::Dot { c: cc, .. } | Item::Wedge { c: cc, .. } => *cc += shift,
                Item::Text { pos, .. } => *pos += shift,
            }
        }
    }
    scene
}

// ===========================================================================
// Flowchart
// ===========================================================================

/// The curve an edge follows, with the outward direction at each end so the
/// markers can be aimed. Kept separate from drawing because the label wants the
/// same curve the stroke used.
struct EdgeGeom {
    p0: Pos2,
    c1: Pos2,
    c2: Pos2,
    p3: Pos2,
    tip_dir: Vec2,
    start_dir: Vec2,
}

#[allow(clippy::too_many_arguments)]
fn edge_geom(a: Pos2, ha: Vec2, b: Pos2, hb: Vec2, forward: bool, down: bool, size: f32) -> EdgeGeom {
    if forward {
        if down {
            let p0 = a + vec2(0.0, ha.y);
            let p3 = b - vec2(0.0, hb.y);
            let k = ((p3.y - p0.y) * 0.42).max(size * 1.1);
            EdgeGeom {
                p0,
                c1: p0 + vec2(0.0, k),
                c2: p3 - vec2(0.0, k),
                p3,
                tip_dir: vec2(0.0, 1.0),
                start_dir: vec2(0.0, -1.0),
            }
        } else {
            let p0 = a + vec2(ha.x, 0.0);
            let p3 = b - vec2(hb.x, 0.0);
            let k = ((p3.x - p0.x) * 0.42).max(size * 1.1);
            EdgeGeom {
                p0,
                c1: p0 + vec2(k, 0.0),
                c2: p3 - vec2(k, 0.0),
                p3,
                tip_dir: vec2(1.0, 0.0),
                start_dir: vec2(-1.0, 0.0),
            }
        }
    } else if down {
        // A back edge, or one that stays inside a layer: route it around the
        // far side instead of straight through the boxes in between.
        let x = a.x.max(b.x) + ha.x.max(hb.x) + size * 1.5;
        let p0 = a + vec2(ha.x, 0.0);
        let p3 = b + vec2(hb.x, 0.0);
        EdgeGeom {
            p0,
            c1: pos2(x, p0.y),
            c2: pos2(x, p3.y),
            p3,
            tip_dir: vec2(-1.0, 0.0),
            start_dir: vec2(1.0, 0.0),
        }
    } else {
        let y = a.y.max(b.y) + ha.y.max(hb.y) + size * 1.5;
        let p0 = a + vec2(0.0, ha.y);
        let p3 = b + vec2(0.0, hb.y);
        EdgeGeom {
            p0,
            c1: pos2(p0.x, y),
            c2: pos2(p3.x, y),
            p3,
            tip_dir: vec2(0.0, -1.0),
            start_dir: vec2(0.0, 1.0),
        }
    }
}

fn flow_layout(c: &mut Canvas<'_>, g: &Graph, dir: Dir, size: f32) -> Scene {
    layered_layout(c, flow_boxes(g, size), &g.edges, dir, size)
}

/// The shared engine: rank, order, place, route, draw.
fn layered_layout(
    c: &mut Canvas<'_>,
    mut boxes: Vec<Box>,
    edges: &[Edge],
    dir: Dir,
    size: f32,
) -> Scene {
    let n = boxes.len();
    size_boxes(c, &mut boxes, size);

    let pairs: Vec<(usize, usize)> = edges.iter().map(|e| (e.from, e.to)).collect();
    let rank = rank_nodes(n, &pairs);
    let max_rank = rank.iter().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_rank + 1];
    for v in 0..n {
        layers[rank[v]].push(v);
    }
    order_layers(&mut layers, &pairs);

    let down = matches!(dir, Dir::Down | Dir::Up);

    // Edge labels live in the gap between two layers, on an opaque pad. If the
    // gap is only a few pixels the pad paints over the boxes on either side and
    // the diagram looks like one smeared block, so each gap is widened by the
    // tallest label that has to fit in it.
    let gaps: Vec<f32> = (0..layers.len())
        .map(|li| {
            let mut need = 0.0f32;
            for e in edges {
                if e.label.is_empty() || rank[e.from] != li || rank[e.to] != li + 1 {
                    continue;
                }
                need = need.max(size * 1.7 + size * 0.36);
            }
            size * 1.25 + need
        })
        .collect();
    place_boxes(&mut boxes, &layers, down, &gaps, size * 0.9);

    // --- edges first, so the boxes paint over the overshoot ---------------
    let geoms: Vec<EdgeGeom> = edges
        .iter()
        .map(|e| {
            let ha = vec2(boxes[e.from].w, boxes[e.from].h) * 0.5;
            let hb = vec2(boxes[e.to].w, boxes[e.to].h) * 0.5;
            edge_geom(
                boxes[e.from].center,
                ha,
                boxes[e.to].center,
                hb,
                rank[e.to] > rank[e.from],
                down,
                size,
            )
        })
        .collect();

    for (e, geo) in edges.iter().zip(&geoms) {
        let dashed = e.style == EdgeStyle::Dotted;
        let color = c.pal.line;
        let lw = if e.style == EdgeStyle::Thick {
            c.w * 1.8
        } else {
            c.w
        };
        c.curve(geo.p0, geo.c1, geo.c2, geo.p3, color, dashed, lw);
        c.arrowhead(geo.p0, geo.start_dir, e.head, color);
        c.arrowhead(geo.p3, geo.tip_dir, e.tail, color);
    }

    // --- boxes ------------------------------------------------------------
    for b in &boxes {
        draw_box(c, b, size);
    }

    // --- edge labels, on an opaque pad so they stay readable ---------------
    for (k, e) in edges.iter().enumerate() {
        if e.label.is_empty() {
            continue;
        }
        // At the curve's own midpoint, not the straight line between centres:
        // on an S-shaped edge the two are far enough apart to matter.
        let geo = &geoms[k];
        let mid = bez(geo.p0, geo.c1, geo.c2, geo.p3, 0.5);
        let m = c.measure(&e.label, size * 0.82, false, false);
        let pad = vec2(size * 0.34, size * 0.18);
        c.rect(
            Rect::from_center_size(mid, m + pad * 2.0),
            3.0,
            c.pal.node_fill,
            Stroke::NONE,
        );
        c.text(
            &e.label,
            mid,
            size * 0.82,
            c.pal.line_text,
            Anchor::Center,
            false,
            false,
        );
    }

    // --- ER cardinalities, just inside each end ---------------------------
    for (k, e) in edges.iter().enumerate() {
        if e.head_note.is_empty() && e.tail_note.is_empty() {
            continue;
        }
        let geo = &geoms[k];
        let d = (geo.p3 - geo.p0).normalized();
        let perp = vec2(-d.y, d.x);
        let off = perp * (size * 1.15);
        // A short edge would otherwise place the two notes past each other.
        let along = (size * 1.05).min((geo.p3 - geo.p0).length() * 0.34);
        if !e.head_note.is_empty() {
            let at = geo.p0 + d * along + off;
            c.text(
                &e.head_note,
                at,
                size * 0.78,
                c.pal.line_text,
                Anchor::Center,
                false,
                false,
            );
        }
        if !e.tail_note.is_empty() {
            let at = geo.p3 - d * along + off;
            c.text(
                &e.tail_note,
                at,
                size * 0.78,
                c.pal.line_text,
                Anchor::Center,
                false,
                false,
            );
        }
    }

    let mut scene = finish(c);
    match dir {
        Dir::Up => mirror(&mut scene, false, true),
        Dir::Left => mirror(&mut scene, true, false),
        _ => {}
    }
    scene
}

// ===========================================================================
// Pie
// ===========================================================================

fn parse_pie(lines: &[&str]) -> Option<(String, Vec<(String, f32)>)> {
    let mut title = String::new();
    let mut data: Vec<(String, f32)> = Vec::new();
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if k == 0 {
            // `pie`, `pie title X`, `pie showData title X`
            if let Some(i) = line.find("title") {
                title = line[i + "title".len()..].trim().to_string();
            }
            continue;
        }
        if line.starts_with("showData") || line.starts_with("accTitle") || line.starts_with("accDescr")
        {
            continue;
        }
        let Some(colon) = line.rfind(':') else {
            continue;
        };
        let label = clean_label(line[..colon].trim());
        let value: f32 = line[colon + 1..].trim().parse().unwrap_or(0.0);
        if !label.is_empty() && value > 0.0 {
            data.push((label, value));
        }
    }
    if data.is_empty() {
        return None;
    }
    Some((title, data))
}

fn pie_layout(c: &mut Canvas<'_>, title: &str, data: &[(String, f32)], size: f32) -> Scene {
    let total: f32 = data.iter().map(|(_, v)| v).sum();
    let r = size * 5.2;
    let legend_w = data
        .iter()
        .map(|(l, _)| c.measure(l, size, false, false).x)
        .fold(0.0f32, f32::max)
        + size * 7.0;
    let title_h = if title.is_empty() { 0.0 } else { size * 2.2 };
    let top = title_h + size * 0.6;

    if !title.is_empty() {
        c.text(
            title,
            pos2(r + size * 0.4, title_h * 0.5),
            size * 1.05,
            c.pal.node_text,
            Anchor::Center,
            true,
            false,
        );
    }

    let center = pos2(r + size * 0.4, top + r);
    let mut angle = -std::f32::consts::FRAC_PI_2;
    for (k, (label, value)) in data.iter().enumerate() {
        let span = value / total * std::f32::consts::TAU;
        let fill = c.pal.slices[k % c.pal.slices.len()];
        c.items.push(Item::Wedge {
            c: center,
            r,
            a0: angle,
            a1: angle + span,
            fill,
        });
        c.line(center, center + vec2(r * angle.cos(), r * angle.sin()), Color32::WHITE, false);
        c.line(
            center,
            center + vec2(r * (angle + span).cos(), r * (angle + span).sin()),
            Color32::WHITE,
            false,
        );

        // Legend, in slice order.
        let ly = top + size * 0.7 + k as f32 * size * 1.9;
        let lx = 2.0 * r + size * 1.6;
        c.rect(
            Rect::from_min_size(pos2(lx, ly - size * 0.45), vec2(size, size)),
            2.0,
            fill,
            Stroke::NONE,
        );
        c.text(
            &format!("{label}  {:.0}%", value / total * 100.0),
            pos2(lx + size * 1.7, ly + size * 0.05),
            size * 0.94,
            c.pal.node_text,
            Anchor::Left,
            false,
            false,
        );
        angle += span;
    }
    let _ = legend_w;
    finish(c)
}

// ===========================================================================
// Sequence diagram
// ===========================================================================

enum SeqRow {
    Msg {
        from: usize,
        to: usize,
        label: String,
        dashed: bool,
        head: Head,
    },
    Note {
        from: usize,
        to: usize,
        text: String,
    },
}

struct Seq {
    actors: Vec<(String, String)>,
    rows: Vec<SeqRow>,
}

impl Seq {
    fn actor(&mut self, id: &str, alias: Option<String>) -> usize {
        if let Some(k) = self.actors.iter().position(|(a, _)| a == id) {
            if let Some(al) = alias {
                self.actors[k].1 = al;
            }
            return k;
        }
        self.actors
            .push((id.to_string(), alias.unwrap_or_else(|| id.to_string())));
        self.actors.len() - 1
    }
}

fn parse_sequence(lines: &[&str]) -> Option<Seq> {
    let mut s = Seq {
        actors: Vec::new(),
        rows: Vec::new(),
    };
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() || k == 0 {
            continue;
        }
        let kw = line.split_whitespace().next().unwrap_or("");
        if kw == "autonumber" || kw == "box" {
            continue;
        }
        if kw == "participant" || kw == "actor" {
            let rest = line[kw.len()..].trim();
            let (id, alias) = match rest.find(" as ") {
                Some(i) => (
                    rest[..i].trim(),
                    Some(clean_label(rest[i + 4..].trim())),
                ),
                None => (rest, None),
            };
            if !id.is_empty() {
                s.actor(id, alias);
            }
            continue;
        }
        if kw == "Note" {
            // `Note over A,B: text` / `Note right of A: text`
            let Some(colon) = line.find(':') else {
                continue;
            };
            let head = &line[..colon];
            let text = clean_label(line[colon + 1..].trim());
            let targets = head.rsplit(" of ").next().unwrap_or(head);
            let targets = targets.trim_start_matches("over").trim();
            let ids: Vec<&str> = targets.split(',').map(|t| t.trim()).collect();
            if ids.is_empty() || ids[0].is_empty() {
                continue;
            }
            let from = s.actor(ids[0], None);
            let to = s.actor(ids.get(1).copied().unwrap_or(ids[0]), None);
            s.rows.push(SeqRow::Note { from, to, text });
            continue;
        }
        if matches!(kw, "loop" | "alt" | "else" | "opt" | "par" | "and" | "critical" | "end"
            | "activate" | "deactivate" | "rect" | "title")
        {
            // Frame decorations are not drawn yet; skipping them keeps the
            // messages and their order, which is the part that carries meaning.
            continue;
        }
        if let Some(row) = parse_seq_message(&line, &mut s) {
            s.rows.push(row);
        }
    }
    if s.actors.is_empty() {
        return None;
    }
    Some(s)
}

fn parse_seq_message(line: &str, s: &mut Seq) -> Option<SeqRow> {
    let colon = line.find(':');
    let (head, label) = match colon {
        Some(i) => (line[..i].trim(), clean_label(line[i + 1..].trim())),
        None => (line, String::new()),
    };
    let dash = head.find('-')?;
    let from_id = head[..dash].trim();
    if from_id.is_empty() {
        return None;
    }
    let rest = &head[dash..];
    let op: String = rest
        .chars()
        .take_while(|ch| matches!(ch, '-' | '>' | 'x' | ')'))
        .collect();
    let to_id = rest[op.len()..].trim();
    if to_id.is_empty() {
        return None;
    }
    let dashed = op.starts_with("--");
    let head_marker = match op.chars().last() {
        Some('x') => Head::Cross,
        Some('>') | Some(')') => Head::Arrow,
        _ => Head::None,
    };
    let from = s.actor(from_id, None);
    let to = s.actor(to_id, None);
    Some(SeqRow::Msg {
        from,
        to,
        label,
        dashed,
        head: head_marker,
    })
}

fn seq_layout(c: &mut Canvas<'_>, s: &Seq, size: f32) -> Scene {
    let line_h = size * 1.7;
    let pad = size * 0.8;
    let head_h = line_h * 1.9;

    // Column widths. A column has to be wide enough for its own header and for
    // the messages that leave or arrive at it, or the labels collide.
    let mut col_w: Vec<f32> = s
        .actors
        .iter()
        .map(|(_, label)| c.measure(label, size, true, false).x + pad * 2.0)
        .collect();
    let mut gap: Vec<f32> = vec![size * 2.0; s.actors.len().saturating_sub(1)];
    for row in &s.rows {
        if let SeqRow::Msg { from, to, label, .. } = row {
            if from == to {
                continue;
            }
            let (a, b) = ((*from).min(*to), (*from).max(*to));
            let need = (c.measure(label, size * 0.9, false, false).x + pad * 2.0)
                / (b - a) as f32;
            for g in gap.iter_mut().take(b).skip(a) {
                *g = g.max(need);
            }
        }
    }
    let mut cx: Vec<f32> = Vec::with_capacity(s.actors.len());
    let mut x = 0.0f32;
    for i in 0..s.actors.len() {
        x += col_w[i] * 0.5;
        cx.push(x);
        x += col_w[i] * 0.5;
        if i + 1 < s.actors.len() {
            x += gap[i];
        }
    }
    let total_w = x;
    for (i, w) in col_w.iter_mut().enumerate() {
        let _ = i;
        *w = w.max(size * 3.0);
    }

    // Header boxes.
    for (i, (_, label)) in s.actors.iter().enumerate() {
        let w = col_w[i].max(size * 3.0);
        let r = Rect::from_center_size(pos2(cx[i], head_h * 0.5), vec2(w, head_h * 0.78));
        c.rect(
            r,
            4.0,
            c.pal.node_fill,
            Stroke::new(c.w, c.pal.node_stroke),
        );
        c.text(
            label,
            pos2(cx[i], head_h * 0.5),
            size,
            c.pal.node_text,
            Anchor::Center,
            true,
            false,
        );
    }

    let bottom = head_h + s.rows.len() as f32 * line_h + size * 0.6;
    for i in 0..s.actors.len() {
        c.line(
            pos2(cx[i], head_h * 0.78 * 0.5 + head_h * 0.39),
            pos2(cx[i], bottom),
            c.pal.line,
            true,
        );
    }

    for (k, row) in s.rows.iter().enumerate() {
        let y = head_h + k as f32 * line_h + line_h * 0.5;
        match row {
            SeqRow::Msg {
                from,
                to,
                label,
                dashed,
                head,
            } => {
                if from == to {
                    // A self-message loops out to the right and back.
                    let x0 = cx[*from];
                    let w = c.measure(label, size * 0.9, false, false).x + pad * 2.0;
                    let h = line_h * 0.55;
                    c.line(pos2(x0, y - h * 0.4), pos2(x0 + w, y - h * 0.4), c.pal.line, *dashed);
                    c.line(pos2(x0 + w, y - h * 0.4), pos2(x0 + w, y + h * 0.4), c.pal.line, *dashed);
                    c.line(pos2(x0 + w, y + h * 0.4), pos2(x0, y + h * 0.4), c.pal.line, *dashed);
                    c.arrowhead(pos2(x0, y + h * 0.4), vec2(-1.0, 0.0), *head, c.pal.line);
                    c.text(
                        label,
                        pos2(x0 + w * 0.5, y - h * 0.4 - size * 0.85),
                        size * 0.9,
                        c.pal.line_text,
                        Anchor::Center,
                        false,
                        false,
                    );
                    continue;
                }
                let (a, b) = (cx[*from], cx[*to]);
                c.arrowhead(pos2(b, y), vec2(b - a, 0.0), *head, c.pal.line);
                c.line(pos2(a, y), pos2(b, y), c.pal.line, *dashed);
                if !label.is_empty() {
                    c.text(
                        label,
                        pos2((a + b) * 0.5, y - size * 0.85),
                        size * 0.9,
                        c.pal.line_text,
                        Anchor::Center,
                        false,
                        false,
                    );
                }
            }
            SeqRow::Note { from, to, text } => {
                let a = cx[*from];
                let b = cx[*to];
                let w = (a.max(b) - a.min(b)) + size * 5.0;
                let tw = c.measure(text, size * 0.9, false, false);
                let r = Rect::from_center_size(
                    pos2((a + b) * 0.5, y),
                    vec2(w.max(tw.x + pad * 2.0), line_h * 0.86),
                );
                c.rect(
                    r,
                    4.0,
                    c.pal.band,
                    Stroke::new(c.w * 0.8, c.pal.node_stroke),
                );
                c.text(
                    text,
                    r.center(),
                    size * 0.9,
                    c.pal.node_text,
                    Anchor::Center,
                    false,
                    false,
                );
            }
        }
    }
    let _ = total_w;
    finish(c)
}

// ===========================================================================
// Class diagram
// ===========================================================================

fn class_marker(op: &str) -> (Head, Head, EdgeStyle) {
    let dotted = op.contains("..");
    let style = if dotted {
        EdgeStyle::Dotted
    } else {
        EdgeStyle::Solid
    };
    // The marker hangs off the end whose side the glyph sits on.
    let (mut a, mut b) = (Head::None, Head::None);
    if op.starts_with("<|") {
        a = Head::Triangle;
    }
    if op.ends_with("|>") {
        b = Head::Triangle;
    }
    if op.starts_with('*') {
        a = Head::Diamond;
    }
    if op.ends_with('*') {
        b = Head::Diamond;
    }
    if op.starts_with('o') {
        a = Head::Circle;
    }
    if op.ends_with('o') {
        b = Head::Circle;
    }
    if op.starts_with('<') && a == Head::None {
        a = Head::Arrow;
    }
    if op.ends_with('>') && b == Head::None {
        b = Head::Arrow;
    }
    (a, b, style)
}

struct ClassModel {
    names: Vec<String>,
    members: Vec<Vec<String>>,
    edges: Vec<Edge>,
}

impl ClassModel {
    fn index(&mut self, name: &str) -> usize {
        if let Some(k) = self.names.iter().position(|n| n == name) {
            return k;
        }
        self.names.push(name.to_string());
        self.members.push(Vec::new());
        self.names.len() - 1
    }
}

fn parse_class(lines: &[&str]) -> Option<ClassModel> {
    let mut m = ClassModel {
        names: Vec::new(),
        members: Vec::new(),
        edges: Vec::new(),
    };
    let mut open: Option<usize> = None;
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() || k == 0 {
            continue;
        }
        if let Some(idx) = open {
            if line == "}" {
                open = None;
                continue;
            }
            m.members[idx].push(line.clone());
            continue;
        }
        if line.starts_with("class ") {
            let rest = line["class ".len()..].trim();
            if let Some(brace) = rest.find('{') {
                let name = rest[..brace].trim();
                let idx = m.index(name);
                open = Some(idx);
            } else if let Some(colon) = rest.find(':') {
                // `class Foo : +member`
                let idx = m.index(rest[..colon].trim());
                m.members[idx].push(rest[colon + 1..].trim().to_string());
            } else {
                let name = rest.split_whitespace().next().unwrap_or("").to_string();
                if !name.is_empty() {
                    m.index(&name);
                }
            }
            continue;
        }
        if let Some(colon) = line.find(':') {
            // `Foo : +member` — but only when the left side is a bare name and
            // the colon is not part of a relation label.
            let left = line[..colon].trim();
            if !left.is_empty() && !left.contains(' ') {
                let idx = m.index(left);
                m.members[idx].push(line[colon + 1..].trim().to_string());
                continue;
            }
        }
        parse_class_relation(&line, &mut m);
    }
    if m.names.is_empty() {
        return None;
    }
    Some(m)
}

fn parse_class_relation(line: &str, m: &mut ClassModel) {
    let cs: Vec<char> = line.chars().collect();
    let mut i = 0;
    let Some(a) = read_plain_name(&cs, &mut i) else {
        return;
    };
    skip_ws(&cs, &mut i);
    let op_start = i;
    while matches!(
        cs.get(i),
        Some('<') | Some('>') | Some('|') | Some('*') | Some('o') | Some('-') | Some('.')
    ) {
        i += 1;
    }
    if i == op_start {
        return;
    }
    let op: String = cs[op_start..i].iter().collect();
    skip_ws(&cs, &mut i);
    let Some(b) = read_plain_name(&cs, &mut i) else {
        return;
    };
    skip_ws(&cs, &mut i);
    let mut label = String::new();
    if cs.get(i) == Some(&':') {
        i += 1;
        label = clean_label(cs[i..].iter().collect::<String>().trim());
    }
    let (head, tail, style) = class_marker(&op);
    let from = m.index(&a);
    let to = m.index(&b);
    m.edges.push(Edge::new(from, to, label, style, head, tail));
}

fn read_plain_name(cs: &[char], i: &mut usize) -> Option<String> {
    let mut s = String::new();
    while let Some(&c) = cs.get(*i) {
        if c.is_alphanumeric() || c == '_' || !c.is_ascii() {
            s.push(c);
            *i += 1;
        } else {
            break;
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn class_layout(c: &mut Canvas<'_>, m: &ClassModel, size: f32) -> Scene {
    let boxes: Vec<Box> = m
        .names
        .iter()
        .enumerate()
        .map(|(k, name)| {
            let mut lines = vec![BoxLine {
                text: name.clone(),
                size,
                bold: true,
                mono: false,
                left: false,
                space_before: 0.0,
            }];
            for mem in &m.members[k] {
                lines.push(BoxLine {
                    text: mem.clone(),
                    size: size * 0.86,
                    bold: false,
                    mono: true,
                    left: true,
                    space_before: 0.0,
                });
            }
            Box {
                // A blank line under the title separates it from the members.
                lines,
                shape: NodeShape::Rect,
                band: !m.members[k].is_empty(),
                w: 0.0,
                h: 0.0,
                center: Pos2::ZERO,
            }
        })
        .collect();
    layered_layout(c, boxes, &m.edges, Dir::Down, size)
}

// ===========================================================================
// State diagram
// ===========================================================================

fn parse_state(lines: &[&str]) -> Option<Graph> {
    let mut g = Graph::default();
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() || k == 0 {
            continue;
        }
        let kw = line.split_whitespace().next().unwrap_or("");
        if matches!(kw, "state" | "direction" | "note" | "classDef" | "class") {
            continue;
        }
        let Some(row) = parse_state_line(&line, &mut g) else {
            continue;
        };
        g.edges.push(row);
    }
    if g.nodes.is_empty() {
        return None;
    }
    Some(g)
}

/// One `A --> B: label`. `[*]` becomes a start marker as a source and an end
/// marker as a target, which is how Mermaid tells the two apart.
fn parse_state_line(line: &str, g: &mut Graph) -> Option<Edge> {
    let (from_part, rest) = match line.find("-->") {
        Some(i) => (line[..i].trim(), &line[i + 3..]),
        None => return None,
    };
    let (to_part, label) = match rest.find(':') {
        Some(i) => (rest[..i].trim(), clean_label(rest[i + 1..].trim())),
        None => (rest.trim(), String::new()),
    };
    let from = state_node(g, from_part, true);
    let to = state_node(g, to_part, false);
    Some(Edge::new(
        from,
        to,
        label,
        EdgeStyle::Solid,
        Head::None,
        Head::Arrow,
    ))
}

fn state_node(g: &mut Graph, name: &str, as_source: bool) -> usize {
    if name == "[*]" {
        return if as_source {
            g.node("__start", NodeShape::Start, String::new())
        } else {
            g.node("__end", NodeShape::End, String::new())
        };
    }
    let shape = if name.starts_with("<<") { NodeShape::Round } else { NodeShape::Round };
    g.node(name, shape, String::new())
}

// ===========================================================================
// ER diagram
// ===========================================================================

struct ErModel {
    names: Vec<String>,
    attrs: Vec<Vec<String>>,
    edges: Vec<Edge>,
    /// Crow's-foot cardinality at the source and target ends of each edge.
    card: Vec<(String, String)>,
}

impl ErModel {
    fn index(&mut self, name: &str) -> usize {
        if let Some(k) = self.names.iter().position(|n| n == name) {
            return k;
        }
        self.names.push(name.to_string());
        self.attrs.push(Vec::new());
        self.names.len() - 1
    }
}

/// `||--o{` and friends, as a readable pair of labels.
fn er_cardinality(token: &str) -> (String, String) {
    let (l, r) = match token.find("--") {
        Some(i) => (&token[..i], &token[i + 2..]),
        None => (token, ""),
    };
    let one = |t: &str| -> String {
        match t {
            "||" => "1".into(),
            "|o" | "o|" => "0..1".into(),
            "}o" | "o{" => "0..N".into(),
            "}|" | "|{" => "1..N".into(),
            "" => String::new(),
            other => other.to_string(),
        }
    };
    (one(l), one(r))
}

fn parse_er(lines: &[&str]) -> Option<ErModel> {
    let mut m = ErModel {
        names: Vec::new(),
        attrs: Vec::new(),
        edges: Vec::new(),
        card: Vec::new(),
    };
    let mut open: Option<usize> = None;
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() || k == 0 {
            continue;
        }
        if let Some(idx) = open {
            if line == "}" {
                open = None;
                continue;
            }
            let cleaned = line
                .trim_start_matches('"')
                .split('"')
                .next()
                .unwrap_or(&line)
                .trim()
                .to_string();
            m.attrs[idx].push(cleaned);
            continue;
        }
        // An entity block opens only on a line *ending* in `{`: a relation line
        // such as `DOCUMENT ||--o{ BLOCK` also contains one, and treating that
        // as a block would swallow the rest of the diagram.
        if line.ends_with('{') {
            let name = line[..line.len() - 1].trim().trim_start_matches('"');
            let idx = m.index(name);
            open = Some(idx);
            continue;
        }
        let cs: Vec<char> = line.chars().collect();
        let mut i = 0;
        let Some(a) = read_plain_name(&cs, &mut i) else {
            continue;
        };
        skip_ws(&cs, &mut i);
        let op_start = i;
        while matches!(
            cs.get(i),
            Some('|') | Some('o') | Some('{') | Some('}') | Some('-') | Some('.')
        ) {
            i += 1;
        }
        if i == op_start {
            continue;
        }
        let op: String = cs[op_start..i].iter().collect();
        skip_ws(&cs, &mut i);
        let Some(b) = read_plain_name(&cs, &mut i) else {
            continue;
        };
        // The label separator is ` : `, so the space has to be skipped before
        // the colon is looked for — otherwise `B : contains` reads as no label.
        skip_ws(&cs, &mut i);
        let mut label = String::new();
        if cs.get(i) == Some(&':') {
            i += 1;
            label = clean_label(cs[i..].iter().collect::<String>().trim());
        }
        let (from, to) = (m.index(&a), m.index(&b));
        // An ER relation is read as crow's feet, not as an arrow, so the ends
        // carry cardinality text instead of a marker.
        let (head_note, tail_note) = er_cardinality(&op);
        let mut edge = Edge::new(
            from,
            to,
            label,
            if op.contains("..") {
                EdgeStyle::Dotted
            } else {
                EdgeStyle::Solid
            },
            Head::None,
            Head::None,
        );
        edge.head_note = head_note;
        edge.tail_note = tail_note;
        m.edges.push(edge);
        m.card.push(er_cardinality(&op));
    }
    if m.names.is_empty() {
        return None;
    }
    Some(m)
}

fn er_layout(c: &mut Canvas<'_>, m: &ErModel, size: f32) -> Scene {
    let boxes: Vec<Box> = m
        .names
        .iter()
        .enumerate()
        .map(|(k, name)| {
            let mut lines = vec![BoxLine {
                text: name.clone(),
                size,
                bold: true,
                mono: false,
                left: false,
                space_before: 0.0,
            }];
            for a in &m.attrs[k] {
                lines.push(BoxLine {
                    text: a.clone(),
                    size: size * 0.86,
                    bold: false,
                    mono: true,
                    left: true,
                    space_before: 0.0,
                });
            }
            Box {
                lines,
                shape: NodeShape::Rect,
                band: !m.attrs[k].is_empty(),
                w: 0.0,
                h: 0.0,
                center: Pos2::ZERO,
            }
        })
        .collect();
    layered_layout(c, boxes, &m.edges, Dir::Down, size)
}

// ===========================================================================
// Gantt
// ===========================================================================

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy as i32;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i32) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

struct GanttTask {
    section: String,
    name: String,
    start: f32,
    len: f32,
    id: String,
    status: String,
}

fn parse_gantt(lines: &[&str]) -> Option<(String, Vec<GanttTask>)> {
    let mut title = String::new();
    let mut tasks: Vec<GanttTask> = Vec::new();
    let mut section = String::new();
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw).trim().to_string();
        if line.is_empty() || k == 0 {
            continue;
        }
        let kw = line.split_whitespace().next().unwrap_or("");
        if kw == "title" {
            title = line[5..].trim().to_string();
            continue;
        }
        if matches!(kw, "dateFormat" | "axisFormat" | "excludes" | "todayMarker" | "tickInterval") {
            continue;
        }
        if kw == "section" {
            section = line[7..].trim().to_string();
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let name = clean_label(line[..colon].trim());
        let fields: Vec<String> = line[colon + 1..]
            .split(',')
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();
        // Leading status words and a leading id are both optional.
        let mut it = fields.iter().peekable();
        let mut status = String::new();
        while let Some(f) = it.peek() {
            if matches!(
                f.as_str(),
                "done" | "active" | "crit" | "milestone" | "after"
            ) {
                status = f.to_string();
                if *f == "after" {
                    break;
                }
                it.next();
            } else {
                break;
            }
        }
        let rest: Vec<&str> = it.map(|s| s.as_str()).collect();
        let looks_like_id = |s: &str| -> bool {
            !s.is_empty() && !s.contains('-') && s != "after" && !s.chars().next().unwrap().is_ascii_digit()
        };
        // `:id, start, len` vs `:start, len`
        let (id, args): (String, Vec<&str>) = if rest.len() >= 3 && looks_like_id(rest[0]) {
            (rest[0].to_string(), rest[1..].to_vec())
        } else {
            (String::new(), rest.to_vec())
        };
        let start = args.first().copied().unwrap_or("").to_string();
        let len_s = args.get(1).copied().unwrap_or("1d");
        // Duration is resolved below, once `after` references can be followed.
        let len = parse_duration(len_s).unwrap_or(1.0);
        tasks.push(GanttTask {
            section: section.clone(),
            name,
            start: f32::NAN,
            len,
            id,
            status: format!("{status}|{start}"),
        });
    }
    if tasks.is_empty() {
        return None;
    }

    // Resolve starts: an explicit date, or `after <id>`. Ids only become
    // resolvable once the task they name has been placed, so this runs in file
    // order and falls back to 0 for a forward reference.
    let mut resolved: std::collections::HashMap<String, f32> = std::collections::HashMap::new();
    for t in &mut tasks {
        let spec = t.status.split('|').nth(1).unwrap_or("").trim().to_string();
        let start = if let Some(dep) = spec.strip_prefix("after ") {
            resolved.get(dep.trim()).copied().unwrap_or(0.0)
        } else if let Some(d) = parse_date(&spec) {
            d as f32
        } else {
            0.0
        };
        t.start = start;
        if !t.id.is_empty() {
            resolved.insert(t.id.clone(), start + t.len);
        }
    }
    Some((title, tasks))
}

/// `7d`, `2w`, `3h`, `30m`, or a bare number of days.
fn parse_duration(s: &str) -> Option<f32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, unit) = match s.find(|c: char| c.is_alphabetic()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, "d"),
    };
    let n: f32 = num.trim().parse().ok()?;
    Some(match unit.trim() {
        "w" => n * 7.0,
        "h" => n / 24.0,
        "m" => n / 1440.0,
        _ => n,
    })
}

fn parse_date(s: &str) -> Option<i32> {
    let s = s.trim();
    let mut it = s.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    Some(days_from_civil(y, m, d))
}

fn gantt_layout(c: &mut Canvas<'_>, title: &str, tasks: &[GanttTask], size: f32) -> Scene {
    let line_h = size * 1.7;
    let row_h = line_h * 1.25;
    let left_w = tasks
        .iter()
        .map(|t| c.measure(&t.name, size * 0.9, false, false).x)
        .fold(size * 8.0, f32::max)
        + size * 3.0;

    let min = tasks.iter().map(|t| t.start).fold(f32::MAX, f32::min);
    let max = tasks
        .iter()
        .map(|t| t.start + t.len)
        .fold(f32::MIN, f32::max);
    let span = (max - min).max(1.0);
    let chart_w = (span * size * 0.9).clamp(size * 14.0, size * 34.0);

    let title_h = if title.is_empty() { 0.0 } else { size * 2.4 };
    if !title.is_empty() {
        c.text(
            title,
            pos2(left_w + chart_w * 0.5, title_h * 0.5),
            size * 1.05,
            c.pal.node_text,
            Anchor::Center,
            true,
            false,
        );
    }

    let mut y = title_h + size * 0.4;
    let mut last_section = String::from("\u{0}");
    let chart_top = y;
    for t in tasks {
        if t.section != last_section {
            // Section header row.
            c.text(
                &t.section,
                pos2(0.0, y + row_h * 0.5),
                size * 0.9,
                c.pal.line_text,
                Anchor::Left,
                true,
                false,
            );
            y += row_h;
            last_section = t.section.clone();
        }
        c.text(
            &t.name,
            pos2(size * 1.4, y + row_h * 0.5),
            size * 0.9,
            c.pal.node_text,
            Anchor::Left,
            false,
            false,
        );
        let x0 = left_w + (t.start - min) / span * chart_w;
        let w = (t.len / span * chart_w).max(size * 0.5);
        let r = Rect::from_min_size(pos2(x0, y + row_h * 0.18), vec2(w, row_h * 0.64));
        let done = t.status.starts_with("done");
        let (fill, stroke) = if done {
            (c.pal.band, Stroke::new(c.w * 0.8, c.pal.node_stroke))
        } else {
            (c.pal.slices[0], Stroke::NONE)
        };
        c.rect(r, 3.0, fill, stroke);
        if !done {
            c.rect(
                Rect::from_min_size(r.min, vec2(r.width() * 0.55, r.height())),
                3.0,
                fill,
                Stroke::NONE,
            );
        }
        y += row_h;
    }

    // Axis: a tick a week, labelled with the date.
    let axis_y = y + size * 0.3;
    c.line(
        pos2(left_w, axis_y),
        pos2(left_w + chart_w, axis_y),
        c.pal.line,
        false,
    );
    let ticks = ((span / 7.0).ceil() as usize).clamp(2, 10);
    for k in 0..=ticks {
        let f = k as f32 / ticks as f32;
        let x = left_w + f * chart_w;
        c.line(pos2(x, axis_y), pos2(x, axis_y + size * 0.45), c.pal.line, false);
        let day = min.round() as i32 + (f * span).round() as i32;
        let (yy, mm, dd) = civil_from_days(day);
        c.text(
            &format!("{mm:02}/{dd:02}"),
            pos2(x, axis_y + size * 1.15),
            size * 0.78,
            c.pal.line_text,
            Anchor::Center,
            false,
            false,
        );
        let _ = yy;
    }
    let _ = chart_top;
    finish(c)
}

// ===========================================================================
// Mindmap
// ===========================================================================

struct MindNode {
    label: String,
    shape: NodeShape,
    children: Vec<usize>,
}

fn parse_mindmap(lines: &[&str]) -> Option<Vec<MindNode>> {
    let mut nodes: Vec<MindNode> = Vec::new();
    // (indent, index) of the open ancestors, innermost last.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for (k, raw) in lines.iter().enumerate() {
        let line = strip_comment(raw);
        if line.trim().is_empty() || k == 0 {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let text = line.trim().to_string();
        let (shape, label) = split_mindmap_label(&text);
        let idx = nodes.len();
        nodes.push(MindNode {
            label,
            shape,
            children: Vec::new(),
        });
        while let Some(&(ind, _)) = stack.last() {
            if ind >= indent {
                stack.pop();
            } else {
                break;
            }
        }
        match stack.last() {
            Some(&(_, parent)) => nodes[parent].children.push(idx),
            None => {}
        }
        stack.push((indent, idx));
    }
    if nodes.is_empty() {
        return None;
    }
    // A mindmap has exactly one root; anything else means we misread it.
    let mut has_parent = vec![false; nodes.len()];
    for n in &nodes {
        for &ch in &n.children {
            has_parent[ch] = true;
        }
    }
    if has_parent.iter().filter(|p| !**p).count() != 1 {
        return None;
    }
    Some(nodes)
}

/// `id((text))`, `id[text]`, `id{{text}}` or a bare label.
///
/// The id in front of the shape is optional and is not part of the text, so
/// `root((rustmd))` has to read as `rustmd` in a circle — looking only at the
/// first character would leave the whole thing as the label.
fn split_mindmap_label(text: &str) -> (NodeShape, String) {
    let pairs = [
        ("((", "))", NodeShape::Circle),
        ("{{", "}}", NodeShape::Hexagon),
        ("[", "]", NodeShape::Rect),
        ("(", ")", NodeShape::Round),
    ];
    for (open, close, shape) in pairs {
        let Some(i) = text.find(open) else { continue };
        if !text.ends_with(close) || i + open.len() >= text.len() - close.len() {
            continue;
        }
        let inner = &text[i + open.len()..text.len() - close.len()];
        return (shape, clean_label(inner));
    }
    (NodeShape::Round, clean_label(text))
}

fn mindmap_layout(c: &mut Canvas<'_>, nodes: &[MindNode], size: f32) -> Scene {
    let line_h = size * 1.5;
    let gap_y = size * 0.7;
    let gap_x = size * 2.6;

    let mut w = vec![0.0f32; nodes.len()];
    let mut h = vec![0.0f32; nodes.len()];
    for (k, n) in nodes.iter().enumerate() {
        let m = c.measure(&n.label, size * 0.95, k == 0, false);
        w[k] = m.x.max(size * 1.6) + size * 1.6;
        h[k] = m.y.max(line_h) + size * 0.7;
    }

    // Subtree heights, then a DFS that hands each node a y range.
    let mut sub = vec![0.0f32; nodes.len()];
    for k in (0..nodes.len()).rev() {
        let kids: f32 = nodes[k]
            .children
            .iter()
            .map(|&ch| sub[ch])
            .sum::<f32>()
            + gap_y * (nodes[k].children.len().saturating_sub(1)) as f32;
        sub[k] = h[k].max(kids);
    }
    let root = (0..nodes.len())
        .find(|k| !nodes.iter().any(|n| n.children.contains(k)))
        .unwrap_or(0);

    let mut pos = vec![Pos2::ZERO; nodes.len()];
    /// Depth-first placement: a node's children stack down its right-hand side,
    /// and the node itself is centred on the block they occupy.
    #[allow(clippy::too_many_arguments)]
    fn place(
        k: usize,
        x: f32,
        top: f32,
        nodes: &[MindNode],
        h: &[f32],
        sub: &[f32],
        w: &[f32],
        pos: &mut [Pos2],
        gap_y: f32,
        gap_x: f32,
    ) {
        let kids_h: f32 = nodes[k].children.iter().map(|&c| sub[c]).sum::<f32>()
            + gap_y * (nodes[k].children.len().saturating_sub(1)) as f32;
        let block = kids_h.max(h[k]);
        pos[k] = pos2(x + w[k] * 0.5, top + block * 0.5);
        let mut child_top = top;
        for &c in &nodes[k].children {
            place(c, x + w[k] + gap_x, child_top, nodes, h, sub, w, pos, gap_y, gap_x);
            child_top += sub[c] + gap_y;
        }
    }
    place(root, 0.0, 0.0, nodes, &h, &sub, &w, &mut pos, gap_y, gap_x);

    // Edges first, then boxes, then labels.
    for (k, n) in nodes.iter().enumerate() {
        for &child in &n.children {
            let a = pos[k] + vec2(w[k] * 0.5, 0.0);
            let b = pos[child] - vec2(w[child] * 0.5, 0.0);
            let k1 = (b.x - a.x) * 0.45;
            c.curve(a, a + vec2(k1, 0.0), b - vec2(k1, 0.0), b, c.pal.node_stroke, false, c.w);
        }
    }
    for (k, n) in nodes.iter().enumerate() {
        let r = Rect::from_center_size(pos[k], vec2(w[k], h[k]));
        let fill = if k == root { c.pal.band } else { c.pal.node_fill };
        match n.shape {
            NodeShape::Circle => {
                let d = w[k].max(h[k]);
                c.dot(pos[k], d * 0.5, fill, Stroke::new(c.w, c.pal.node_stroke));
            }
            NodeShape::Hexagon => {
                let kk = h[k] * 0.5;
                c.poly(
                    vec![
                        pos2(r.left() + kk, r.top()),
                        pos2(r.right() - kk, r.top()),
                        pos2(r.right(), r.center().y),
                        pos2(r.right() - kk, r.bottom()),
                        pos2(r.left() + kk, r.bottom()),
                        pos2(r.left(), r.center().y),
                    ],
                    fill,
                    Stroke::new(c.w, c.pal.node_stroke),
                );
            }
            NodeShape::Rect => {
                c.rect(r, 3.0, fill, Stroke::new(c.w, c.pal.node_stroke));
            }
            _ => {
                c.rect(r, h[k] * 0.5, fill, Stroke::new(c.w, c.pal.node_stroke));
            }
        }
        c.text(
            &n.label,
            pos[k],
            size * 0.95,
            c.pal.node_text,
            Anchor::Center,
            k == root,
            false,
        );
    }
    finish(c)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;

    fn headless() -> egui::Context {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        Theme::light().apply(&ctx);
        ctx
    }

    /// Lay `src` out with a real (headless) `Ui`, exactly as the render path
    /// does. `None` is a meaningful answer: it is what sends the caller back to
    /// drawing a code block.
    fn scene(src: &str) -> Option<Scene> {
        let ctx = headless();
        let mut out = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 900.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = build(ui, src, &Theme::light(), 16.0);
                });
            },
        );
        out
    }

    fn parsed(src: &str) -> Graph {
        let lines: Vec<&str> = src.lines().collect();
        parse_flow(&lines).expect("should parse")
    }

    /// Exposed so the `dump` module can lay out the same samples.
    pub(super) fn scene_for_test(src: &str) -> Option<Scene> {
        scene(src)
    }

    /// The document these fixtures were copied from. Reading it here means a
    /// diagram that exists in the file but not in `SAMPLES` (or vice versa)
    /// fails the build instead of quietly drifting.
    const DOC: &str = include_str!("../samples/test-render.md");

    /// Pull every fenced block out of `src` whose info string is a Mermaid one.
    fn mermaid_fences(src: &str) -> Vec<String> {
        let lines: Vec<&str> = src.lines().collect();
        let mut out = Vec::new();
        let mut open: Option<(&str, usize)> = None; // (info string, body start)
        for (k, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix("```") else {
                continue;
            };
            match open {
                None => open = Some((rest.trim(), k + 1)),
                Some((info, from)) => {
                    if is_mermaid(info) {
                        out.push(lines[from..k].join("\n"));
                    }
                    open = None;
                }
            }
        }
        out
    }

    /// Every diagram type the module claims to support, exactly as it appears
    /// in the project's own `samples/test-render.md`.
    pub(super) const SAMPLES: &[(&str, &str)] = &[
        ("1.1-flowchart", FLOW),
        (
            "1.2-sequence",
            "sequenceDiagram
    participant U as 用户
    participant App as rustmd
    participant FS as 文件系统
    U->>App: 打开 .md 文件
    App->>FS: 读取字节
    FS-->>App: 返回内容
    App-->>U: 渲染结果",
        ),
        (
            "1.3-class",
            "classDiagram
    class Block {
        +BlockKind kind
        +String content
        +render()
    }
    class Paragraph {
        +Vec~Span~ spans
    }
    Block <|-- Paragraph",
        ),
        (
            "1.4-state",
            "stateDiagram-v2
    [*] --> 编辑中
    编辑中 --> 已保存: Cmd+S
    已保存 --> 编辑中: 继续输入
    已保存 --> [*]",
        ),
        (
            "1.5-gantt",
            "gantt
    title 开发计划
    dateFormat YYYY-MM-DD
    section 解析器
    词法分析    :a1, 2026-01-01, 7d
    块级结构    :after a1, 5d
    section 渲染
    数学排版    :2026-01-10, 10d
    导出 HTML   :2026-01-15, 4d",
        ),
        (
            "1.6-pie",
            "pie title 语法使用占比
    \"正文段落\" : 45
    \"代码块\" : 25
    \"数学公式\" : 20
    \"表格\" : 10",
        ),
        (
            "1.7-er",
            "erDiagram
    DOCUMENT ||--o{ BLOCK : contains
    BLOCK ||--o{ SPAN : contains
    DOCUMENT {
        string path
        int version
    }",
        ),
        (
            "1.8-mindmap",
            "mindmap
  root((rustmd))
    解析
      词法
      块级
    渲染
      文本
      数学
      代码",
        ),
    ];

    const FLOW: &str = "flowchart LR
    A[开始] --> B{条件判断}
    B -- 是 --> C[执行操作]
    B -- 否 --> D[跳过]
    C --> E([结束])
    D --> E";

    #[test]
    fn identifiers_never_swallow_an_arrow() {
        // Ids may not contain `-`, or `A-->B` lexes as the single id `A-->B`.
        let g = parsed(FLOW);
        let ids: Vec<&str> = g.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["A", "B", "C", "D", "E"], "ids were {ids:?}");
        assert_eq!(g.edges.len(), 5, "expected five edges");
        assert_eq!((g.edges[0].from, g.edges[0].to), (0, 1));
        assert_eq!(g.edges[1].label, "是");
        assert_eq!(g.edges[2].label, "否");
        // `E([结束])` is a stadium, mentioned twice, and must stay one node.
        assert_eq!(g.nodes[4].shape, NodeShape::Stadium);
        assert_eq!(g.nodes[4].label, "结束");
    }

    #[test]
    fn every_node_gets_a_box_and_a_label() {
        let s = scene(FLOW).expect("flowchart should lay out");
        assert!(s.size.x > 40.0 && s.size.y > 40.0, "size {:?}", s.size);
        let texts = s.texts();
        for want in ["开始", "条件判断", "执行操作", "跳过", "结束", "是", "否"] {
            assert!(
                texts.iter().any(|t| t == want),
                "{want:?} missing from {texts:?}"
            );
        }
        // A diamond and a stadium are polygons/dots, not plain rects, so the
        // shape vocabulary has to have reached the drawing.
        assert!(s.count(|i| matches!(i, Item::Poly { .. })) >= 1, "no polygon");
    }

    #[test]
    fn edge_label_forms_agree() {
        // `-- label -->` and `-->|label|` mean the same thing.
        let a = parsed("flowchart LR\n  A -->|yes| B");
        let b = parsed("flowchart LR\n  A -- yes --> B");
        assert_eq!(a.edges[0].label, "yes");
        assert_eq!(b.edges[0].label, "yes");
        assert_eq!(a.edges[0].style, EdgeStyle::Solid);
        assert_eq!(b.edges[0].style, EdgeStyle::Solid);

        let c = parsed("flowchart LR\n  A -.-> B");
        assert_eq!(c.edges[0].style, EdgeStyle::Dotted);
        assert_eq!(c.edges[0].tail, Head::Arrow);

        let d = parsed("flowchart TD\n  A ==> B");
        assert_eq!(d.edges[0].style, EdgeStyle::Thick);

        let e = parsed("flowchart TD\n  A --o B");
        assert_eq!(e.edges[0].tail, Head::Circle);
        let f = parsed("flowchart TD\n  A --x B");
        assert_eq!(f.edges[0].tail, Head::Cross);
        let g2 = parsed("flowchart TD\n  A <--> B");
        assert_eq!(g2.edges[0].head, Head::Arrow);
        assert_eq!(g2.edges[0].tail, Head::Arrow);
    }

    #[test]
    fn a_chain_on_one_line_becomes_a_chain_of_edges() {
        let g = parsed("flowchart LR\n  A --> B --> C --> D");
        assert_eq!(g.nodes.len(), 4);
        assert_eq!(g.edges.len(), 3);
        assert_eq!(g.edges[0].to, 1);
        assert_eq!(g.edges[1].from, 1);
        assert_eq!(g.edges[2].to, 3);
    }

    #[test]
    fn ranking_layers_a_chain_and_survives_a_cycle() {
        let r = rank_nodes(4, &[(0, 1), (1, 2), (2, 3)]);
        assert_eq!(r, [0, 1, 2, 3], "a chain must be four layers deep");

        // A cycle cannot be ranked by longest path; the point is that it must
        // not panic, and that the members do not all land on one row.
        let r = rank_nodes(3, &[(0, 1), (1, 2), (2, 0)]);
        assert!(r.iter().any(|&x| x > 0), "{r:?}");
    }

    #[test]
    fn direction_mirrors_the_layout() {
        let down = scene("flowchart TD\n  A --> B --> C").unwrap();
        let right = scene("flowchart LR\n  A --> B --> C").unwrap();
        assert!(
            down.size.y > down.size.x,
            "TD should be tall, got {:?}",
            down.size
        );
        assert!(
            right.size.x > right.size.y,
            "LR should be wide, got {:?}",
            right.size
        );

        // BT is TD flipped, so the first node ends up at the bottom.
        let up = scene("flowchart BT\n  A --> B --> C").unwrap();
        let y_of = |s: &Scene, label: &str| -> f32 {
            s.items
                .iter()
                .find_map(|i| match i {
                    Item::Text { pos, galley, .. } if galley.job.text == label => Some(pos.y),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label} not drawn"))
        };
        assert!(y_of(&down, "A") < y_of(&down, "C"), "TD: A above C");
        assert!(y_of(&up, "A") > y_of(&up, "C"), "BT: A below C");
    }

    #[test]
    fn the_node_shapes_are_recognised() {
        let g = parsed(
            "flowchart TD
  A[rect]
  B(round)
  C([stadium])
  D((circle))
  E{diamond}
  F{{hexagon}}
  G[[subroutine]]
  H[(cylinder)]
  I>asymmetric]
  J[/parallelogram/]",
        );
        let shapes: Vec<NodeShape> = g.nodes.iter().map(|n| n.shape).collect();
        assert_eq!(
            shapes,
            [
                NodeShape::Rect,
                NodeShape::Round,
                NodeShape::Stadium,
                NodeShape::Circle,
                NodeShape::Diamond,
                NodeShape::Hexagon,
                NodeShape::Subroutine,
                NodeShape::Cylinder,
                NodeShape::Asymmetric,
                NodeShape::Parallelogram,
            ]
        );
        for n in &g.nodes {
            assert!(!n.label.is_empty(), "shape syntax leaked into a label");
        }
    }

    #[test]
    fn labels_survive_quoting_and_line_breaks() {
        let g = parsed("flowchart TD\n  A[\"a quoted label\"]\n  B[first<br/>second]");
        assert_eq!(g.nodes[0].label, "a quoted label");
        assert_eq!(g.nodes[1].label, "first\nsecond");
    }

    #[test]
    fn comments_and_styling_lines_are_ignored() {
        let g = parsed(
            "flowchart TD
  %% a comment
  A --> B
  style A fill:#f9f
  classDef big font-size:20px
  linkStyle 0 stroke:#333",
        );
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
    }

    #[test]
    fn an_unknown_diagram_falls_back_instead_of_guessing() {
        // The caller draws a code block when this is `None`, so returning a
        // half-drawn scene here would be worse than useless.
        assert!(scene("this is not a diagram").is_none());
        assert!(scene("").is_none());
        assert!(scene("%% only a comment").is_none());
    }

    #[test]
    fn the_fence_language_is_recognised() {
        assert!(is_mermaid("mermaid"));
        assert!(is_mermaid("Mermaid"));
        assert!(is_mermaid(" mmd "));
        assert!(!is_mermaid("rust"));
        assert!(!is_mermaid(""));
    }

    /// The fixtures are the shapes the project's own test document uses, so
    /// this is the "does it still work end to end" test.
    #[test]
    fn every_documented_diagram_type_lays_out() {
        for (name, src) in SAMPLES {
            let s = scene(src).unwrap_or_else(|| panic!("{name}: did not lay out"));
            assert!(
                s.size.x > 30.0 && s.size.y > 20.0,
                "{name}: degenerate size {:?}",
                s.size
            );
            assert!(s.items.len() > 4, "{name}: only {} items", s.items.len());
            assert!(!s.texts().is_empty(), "{name}: no text drawn");
        }
    }

    /// The fixtures above are hand-copied, so they can rot. This pins them to
    /// the document they came from.
    #[test]
    fn the_fixtures_are_the_diagrams_the_document_actually_uses() {
        let doc_src = mermaid_fences(DOC);
        assert!(
            doc_src.len() >= SAMPLES.len(),
            "the document lost diagrams: {} fences vs {} fixtures",
            doc_src.len(),
            SAMPLES.len()
        );
        for (name, src) in SAMPLES {
            assert!(
                doc_src.iter().any(|f| f.trim() == src.trim()),
                "{name} no longer appears in samples/test-render.md"
            );
        }
    }

    /// The real end-to-end check: whatever the test document contains must lay
    /// out, not fall back to a code block.
    #[test]
    fn every_diagram_in_the_sample_document_lays_out() {
        let fences = mermaid_fences(DOC);
        assert!(fences.len() >= 8, "only {} fences found", fences.len());
        for (k, src) in fences.iter().enumerate() {
            let s = scene(src).unwrap_or_else(|| {
                panic!("fence #{k} fell back to a code block:\n---\n{src}\n---")
            });
            assert!(
                s.size.x > 30.0 && s.size.y > 20.0,
                "fence #{k}: degenerate size {:?}",
                s.size
            );
            assert!(!s.texts().is_empty(), "fence #{k}: nothing labelled");
        }
    }

    #[test]
    fn a_sequence_diagram_uses_the_aliases_and_the_right_arrows() {
        let src = SAMPLES
            .iter()
            .find(|(n, _)| *n == "1.2-sequence")
            .unwrap()
            .1;
        let seq = parse_sequence(&src.lines().collect::<Vec<_>>()).expect("parses");
        assert_eq!(seq.actors.len(), 3);
        assert_eq!(seq.actors[0].1, "用户", "aliases must win over ids");
        assert_eq!(seq.actors[2].1, "文件系统");
        assert_eq!(seq.rows.len(), 4);

        let dashed: Vec<bool> = seq
            .rows
            .iter()
            .map(|r| match r {
                SeqRow::Msg { dashed, .. } => *dashed,
                _ => panic!("only messages here"),
            })
            .collect();
        // `->>` is solid, `-->>` is dashed.
        assert_eq!(dashed, [false, false, true, true]);
    }

    #[test]
    fn a_pie_chart_gets_one_slice_per_row() {
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.6-pie").unwrap().1;
        let (title, data) = parse_pie(&src.lines().collect::<Vec<_>>()).expect("parses");
        assert_eq!(title, "语法使用占比");
        assert_eq!(data.len(), 4);
        assert_eq!(data[0].0, "正文段落");
        assert_eq!(data[0].1, 45.0);

        let s = scene(src).unwrap();
        assert_eq!(s.count(|i| matches!(i, Item::Wedge { .. })), 4);
        let texts = s.texts();
        assert!(texts.iter().any(|t| t == "正文段落  45%"), "{texts:?}");
    }

    #[test]
    fn a_class_box_carries_its_members() {
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.3-class").unwrap().1;
        let m = parse_class(&src.lines().collect::<Vec<_>>()).expect("parses");
        assert_eq!(m.names, ["Block", "Paragraph"]);
        assert_eq!(m.members[0].len(), 3);
        assert_eq!(m.members[0][2], "+render()");

        // `Block <|-- Paragraph` is inheritance: a hollow triangle at Block.
        let e = &m.edges[0];
        assert_eq!((e.from, e.to), (0, 1));
        assert_eq!(e.head, Head::Triangle);
        assert_eq!(e.tail, Head::None);

        let s = scene(src).unwrap();
        let texts = s.texts();
        assert!(texts.iter().any(|t| t == "+BlockKind kind"), "{texts:?}");
    }

    #[test]
    fn a_state_diagram_gets_markers_at_both_ends() {
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.4-state").unwrap().1;
        let g = parse_state(&src.lines().collect::<Vec<_>>()).expect("parses");
        // `[*]` is a start marker as a source and an end marker as a target, so
        // the two uses must not collapse into one node.
        assert!(g.nodes.iter().any(|n| n.id == "__start"));
        assert!(g.nodes.iter().any(|n| n.id == "__end"));
        assert_ne!(
            g.nodes.iter().position(|n| n.id == "__start"),
            g.nodes.iter().position(|n| n.id == "__end")
        );
        assert_eq!(
            g.edges
                .iter()
                .find(|e| e.label == "Cmd+S")
                .map(|e| g.nodes[e.from].id.as_str()),
            Some("编辑中")
        );

        let s = scene(src).unwrap();
        assert!(s.count(|i| matches!(i, Item::Dot { .. })) >= 2, "no end marker");
    }

    #[test]
    fn an_er_relation_reads_as_a_relation_not_as_an_entity_block() {
        // The `{` inside `o{` used to look like the start of an attribute block
        // and swallow everything after it.
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.7-er").unwrap().1;
        let m = parse_er(&src.lines().collect::<Vec<_>>()).expect("parses");
        assert_eq!(m.names, ["DOCUMENT", "BLOCK", "SPAN"]);
        assert_eq!(m.attrs[1].len(), 0, "BLOCK has no attribute block");
        assert_eq!(m.attrs[2].len(), 0, "SPAN has no attribute block");
        assert_eq!(m.edges.len(), 2, "one edge per relation line");
        assert_eq!(m.edges[1].label, "contains");
        assert_eq!(m.edges[0].label, "contains");
        // `DOCUMENT ||--o{ BLOCK` is one-to-many, and the ends must not swap.
        assert_eq!(m.edges[0].head_note, "1");
        assert_eq!(m.edges[0].tail_note, "0..N");

        let s = scene(src).unwrap();
        let texts = s.texts();
        assert!(texts.iter().any(|t| t == "contains"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "string path"), "{texts:?}");
    }

    #[test]
    fn gantt_chains_after_dependencies() {
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.5-gantt").unwrap().1;
        let (title, tasks) = parse_gantt(&src.lines().collect::<Vec<_>>()).expect("parses");
        assert_eq!(title, "开发计划");
        // Two section headers plus four tasks.
        assert_eq!(tasks.len(), 4);
        let by = |name: &str| tasks.iter().find(|t| t.name == name).unwrap();
        let lexer = by("词法分析");
        let blocks = by("块级结构");
        assert_eq!(lexer.len, 7.0);
        assert_eq!(blocks.len, 5.0);
        assert_eq!(
            blocks.start,
            lexer.start + lexer.len,
            "`after a1` must start where a1 ends"
        );
        // A bare `2026-01-15, 4d` has no id and uses the date as its start.
        assert!(by("导出 HTML").start > by("数学排版").start);
    }

    #[test]
    fn dates_round_trip() {
        let d = days_from_civil(2026, 1, 1);
        assert_eq!(civil_from_days(d), (2026, 1, 1));
        assert_eq!(civil_from_days(d + 30), (2026, 1, 31));
        // A leap day has to survive the trip too.
        let l = days_from_civil(2024, 2, 29);
        assert_eq!(civil_from_days(l), (2024, 2, 29));
        assert_eq!(days_from_civil(2024, 3, 1) - l, 1);
    }

    #[test]
    fn a_mindmap_root_drops_its_id_and_keeps_its_shape() {
        let src = SAMPLES.iter().find(|(n, _)| *n == "1.8-mindmap").unwrap().1;
        let nodes = parse_mindmap(&src.lines().collect::<Vec<_>>()).expect("parses");
        // `root((rustmd))` is id + shape; the label is what is inside.
        assert_eq!(nodes[0].label, "rustmd");
        assert_eq!(nodes[0].shape, NodeShape::Circle);
        assert_eq!(nodes[0].children.len(), 2, "解析 / 渲染");
        assert!(nodes.iter().any(|n| n.label == "词法"));

        let s = scene(src).unwrap();
        assert!(s.texts().iter().any(|t| t == "rustmd"));
    }

    #[test]
    fn a_flat_list_is_not_a_mindmap() {
        // Two roots means we misread the indentation, and a wrong tree is worse
        // than an honest code block.
        assert!(parse_mindmap(&"mindmap\n  a\n  b".lines().collect::<Vec<_>>()).is_none());
    }
}

/// Debug aid: turn a `Scene` into an SVG so the layout can be looked at without
/// a screenshot. Only used by the `dump_scenes` test.
#[cfg(test)]
impl Scene {
    pub(crate) fn to_svg(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" \
             viewBox=\"0 0 {:.0} {:.0}\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/>\
             <g font-family=\"PingFang SC,Helvetica,sans-serif\">",
            self.size.x + 20.0,
            self.size.y + 20.0,
            self.size.x + 20.0,
            self.size.y + 20.0
        ));
        let esc = |t: &str| {
            t.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let col = |c: Color32| {
            if c.a() == 0 {
                "none".to_string()
            } else {
                format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
            }
        };
        for it in &self.items {
            match it {
                Item::Rect {
                    r,
                    radius,
                    fill,
                    stroke,
                } => s.push_str(&format!(
                    "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"{:.1}\" \
                     fill=\"{}\" stroke=\"{}\" stroke-width=\"{:.1}\"/>",
                    r.min.x + 10.0,
                    r.min.y + 10.0,
                    r.width(),
                    r.height(),
                    radius,
                    col(*fill),
                    col(stroke.color),
                    stroke.width
                )),
                Item::Poly { pts, fill, stroke } => {
                    let p: Vec<String> = pts
                        .iter()
                        .map(|q| format!("{:.1},{:.1}", q.x + 10.0, q.y + 10.0))
                        .collect();
                    s.push_str(&format!(
                        "<polygon points=\"{}\" fill=\"{}\" stroke=\"{}\" stroke-width=\"{:.1}\"/>",
                        p.join(" "),
                        col(*fill),
                        col(stroke.color),
                        stroke.width
                    ));
                }
                Item::Curve {
                    a,
                    c1,
                    c2,
                    b,
                    stroke,
                    dashed,
                } => s.push_str(&format!(
                    "<path d=\"M {:.1} {:.1} C {:.1} {:.1} {:.1} {:.1} {:.1} {:.1}\" fill=\"none\" \
                     stroke=\"{}\" stroke-width=\"{:.1}\"{}/>",
                    a.x + 10.0,
                    a.y + 10.0,
                    c1.x + 10.0,
                    c1.y + 10.0,
                    c2.x + 10.0,
                    c2.y + 10.0,
                    b.x + 10.0,
                    b.y + 10.0,
                    col(stroke.color),
                    stroke.width,
                    if *dashed {
                        " stroke-dasharray=\"4 3\""
                    } else {
                        ""
                    }
                )),
                Item::Line {
                    a,
                    b,
                    stroke,
                    dashed,
                } => s.push_str(&format!(
                    "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"{}\" \
                     stroke-width=\"{:.1}\"{}/>",
                    a.x + 10.0,
                    a.y + 10.0,
                    b.x + 10.0,
                    b.y + 10.0,
                    col(stroke.color),
                    stroke.width,
                    if *dashed {
                        " stroke-dasharray=\"4 3\""
                    } else {
                        ""
                    }
                )),
                Item::Dot { c, r, fill, stroke } => s.push_str(&format!(
                    "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{:.1}\" fill=\"{}\" stroke=\"{}\" \
                     stroke-width=\"{:.1}\"/>",
                    c.x + 10.0,
                    c.y + 10.0,
                    r,
                    col(*fill),
                    col(stroke.color),
                    stroke.width
                )),
                Item::Wedge { c, r, a0, a1, fill } => {
                    let large = if (a1 - a0).abs() > std::f32::consts::PI { 1 } else { 0 };
                    let p0 = pos2(c.x + r * a0.cos(), c.y + r * a0.sin());
                    let p1 = pos2(c.x + r * a1.cos(), c.y + r * a1.sin());
                    s.push_str(&format!(
                        "<path d=\"M {:.1} {:.1} L {:.1} {:.1} A {:.1} {:.1} 0 {} 1 {:.1} {:.1} Z\" \
                         fill=\"{}\"/>",
                        c.x + 10.0,
                        c.y + 10.0,
                        p0.x + 10.0,
                        p0.y + 10.0,
                        r,
                        r,
                        large,
                        p1.x + 10.0,
                        p1.y + 10.0,
                        col(*fill)
                    ));
                }
                Item::Text { pos, galley, color } => {
                    let size = galley
                        .job
                        .sections
                        .first()
                        .map(|s| s.format.font_id.size)
                        .unwrap_or(12.0);
                    s.push_str(&format!(
                        "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"{:.1}\" fill=\"{}\">{}</text>",
                        pos.x + 10.0,
                        pos.y + size * 0.82 + 10.0,
                        size,
                        col(*color),
                        esc(&galley.job.text)
                    ));
                }
            }
        }
        s.push_str("</g></svg>");
        s
    }
}

#[cfg(test)]
mod dump {
    use super::tests::scene_for_test;
    use std::fs;

    /// Writes every sample diagram to `/tmp/rustmd-mermaid/*.svg` so the layout
    /// can be eyeballed. Ignored by default because it writes to the filesystem.
    #[test]
    #[ignore]
    fn dump_scenes() {
        let dir = "/tmp/rustmd-mermaid";
        let _ = fs::create_dir_all(dir);
        for (name, src) in super::tests::SAMPLES {
            match scene_for_test(src) {
                Some(s) => {
                    let _ = fs::write(format!("{dir}/{name}.svg"), s.to_svg());
                    println!("{name}: {:.0}x{:.0}", s.size.x, s.size.y);
                }
                None => println!("{name}: NOT SUPPORTED"),
            }
        }
    }
}
