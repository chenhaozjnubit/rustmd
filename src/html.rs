//! A small, forgiving HTML renderer.
//!
//! Markdown is allowed to contain raw HTML, and people write real HTML in it:
//! a tinted `<div>` callout, a `<table>`, a `<details>` disclosure. Showing that
//! as a wall of code-coloured tags is technically "handled" and practically
//! useless, so blocks that open with a block-level tag are parsed here and laid
//! out as the structure they describe.
//!
//! The scope is deliberately the subset that Markdown documents actually use —
//! containers with a little inline CSS, tables, images, disclosure widgets —
//! and the rule for everything else is to *degrade*, never to guess:
//!
//! * an unknown **block** element renders as a plain container holding its
//!   children, so its text still appears;
//! * a fragment with no block-level element at all returns `None`, which sends
//!   the caller back to rendering the paragraph normally (where the inline
//!   handling in `parser.rs` takes over).
//!
//! There is no CSS engine. `padding`, `margin`, `border(-left)`,
//! `background(-color)`, `color`, `width` and `text-align` are read off the
//! `style` attribute and anything else is ignored.

use egui::{Align, Color32, CornerRadius, Frame, Margin, Rect, Sense, Stroke, Ui, vec2};

// ===========================================================================
// Attributes and the CSS subset
// ===========================================================================

/// The `value` of `name="value"` in an attribute string.
///
/// Attribute strings arrive as the raw text between the tag name and the `>`:
/// `r#" href="x" class="y""#. Unquoted values (`width=120`) are accepted too,
/// because HTML allows them and hand-written HTML uses them.
pub fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip whitespace and separators.
        if !(bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_') {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
        {
            i += 1;
        }
        let key = &attrs[start..i];
        // Skip whitespace before a possible `=`.
        let mut j = i;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            continue;
        }
        j += 1;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let value = if j < bytes.len() && (bytes[j] == b'"' || bytes[j] == b'\'') {
            let quote = bytes[j];
            j += 1;
            let vs = j;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            let v = attrs[vs..j].to_string();
            j = (j + 1).min(bytes.len());
            v
        } else {
            let vs = j;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            attrs[vs..j].to_string()
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value);
        }
        i = j;
    }
    None
}

/// One declaration out of a `style="…"` attribute.
pub fn css_prop(css: &str, name: &str) -> Option<String> {
    for decl in css.split(';') {
        let Some((k, v)) = decl.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(name) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// A CSS colour: `#rgb`, `#rrggbb`, `rgb(r,g,b)`, or one of the common names.
pub fn parse_color(s: &str) -> Option<Color32> {
    let t = s.trim();
    if let Some(hex) = t.strip_prefix('#') {
        let d = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
        let b = hex.as_bytes();
        return match b.len() {
            3 => Some(Color32::from_rgb(
                d(b[0])? * 17,
                d(b[1])? * 17,
                d(b[2])? * 17,
            )),
            6 | 8 => Some(Color32::from_rgb(
                d(b[0])? * 16 + d(b[1])?,
                d(b[2])? * 16 + d(b[3])?,
                d(b[4])? * 16 + d(b[5])?,
            )),
            _ => None,
        };
    }
    if let Some(rest) = t.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let n: Vec<u8> = rest
            .split(',')
            .filter_map(|p| p.trim().parse::<f32>().ok().map(|v| v.clamp(0.0, 255.0) as u8))
            .collect();
        if n.len() >= 3 {
            return Some(Color32::from_rgb(n[0], n[1], n[2]));
        }
        return None;
    }
    match t.to_ascii_lowercase().as_str() {
        "red" => Some(Color32::from_rgb(0xd3, 0x2f, 0x2f)),
        "blue" => Some(Color32::from_rgb(0x1d, 0x6f, 0xd6)),
        "green" => Some(Color32::from_rgb(0x1a, 0x7f, 0x37)),
        "orange" => Some(Color32::from_rgb(0xd9, 0x6a, 0x0b)),
        "purple" => Some(Color32::from_rgb(0x7a, 0x3f, 0xc0)),
        "gray" | "grey" => Some(Color32::from_rgb(0x77, 0x77, 0x77)),
        "black" => Some(Color32::from_rgb(0x1a, 0x1a, 0x1a)),
        "white" => Some(Color32::from_rgb(0xff, 0xff, 0xff)),
        _ => None,
    }
}

/// [`parse_color`] packed as `0xRRGGBB`, for callers that must not depend on
/// the UI toolkit.
pub fn parse_color_packed(s: &str) -> Option<u32> {
    let c = parse_color(s)?;
    Some(((c.r() as u32) << 16) | ((c.g() as u32) << 8) | c.b() as u32)
}

/// A CSS length. `px` and bare numbers are pixels; `em`/`rem` scale with the
/// current font size; `%` is taken against the available width by the caller.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Len {
    Px(f32),
    Em(f32),
    Pct(f32),
}

impl Len {
    pub fn px(self, em: f32) -> f32 {
        match self {
            Len::Px(v) => v,
            Len::Em(v) => v * em,
            Len::Pct(v) => v * 0.01 * em,
        }
    }
}

pub fn parse_len(s: &str) -> Option<Len> {
    let t = s.trim();
    if let Some(v) = t.strip_suffix("px") {
        return v.trim().parse().ok().map(Len::Px);
    }
    if let Some(v) = t.strip_suffix("rem").or_else(|| t.strip_suffix("em")) {
        return v.trim().parse().ok().map(Len::Em);
    }
    if let Some(v) = t.strip_suffix('%') {
        return v.trim().parse().ok().map(Len::Pct);
    }
    if t.eq_ignore_ascii_case("auto") {
        return None;
    }
    t.parse().ok().map(Len::Px)
}

/// A `border` / `border-left` shorthand: `<width> <style> <color>` in any order.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Border {
    pub width: f32,
    pub color: Option<Color32>,
    pub none: bool,
}

pub fn parse_border(s: &str) -> Border {
    let mut b = Border {
        width: 0.0,
        color: None,
        none: false,
    };
    for part in s.split_whitespace() {
        if let Some(l) = parse_len(part) {
            b.width = l.px(16.0);
            continue;
        }
        if let Some(c) = parse_color(part) {
            b.color = Some(c);
            continue;
        }
        match part.to_ascii_lowercase().as_str() {
            "none" | "hidden" => {
                b.none = true;
                b.width = 0.0;
            }
            // A style keyword with no explicit width means the CSS initial
            // width, which is `medium` — 3px is the conventional stand-in.
            "solid" | "dashed" | "dotted" | "double" if b.width == 0.0 => b.width = 3.0,
            _ => {}
        }
    }
    b
}

/// The styling an element contributes to its own box.
#[derive(Debug, Clone, Default)]
pub struct BoxStyle {
    pub background: Option<Color32>,
    pub padding: egui::Margin,
    pub margin: egui::Margin,
    pub border: Option<Border>,
    pub border_left: Option<Border>,
    pub width: Option<Len>,
    pub align: Option<Align>,
    pub color: Option<Color32>,
}

/// Read the `style` attribute (plus a couple of presentational HTML attributes)
/// into a [`BoxStyle`].
pub fn box_style(attrs: &str, em: f32) -> BoxStyle {
    let mut out = BoxStyle::default();
    let css = attr_value(attrs, "style").unwrap_or_default();

    if let Some(v) = css_prop(&css, "background").or_else(|| css_prop(&css, "background-color")) {
        // `background: #fff url(…)` — take the colour, ignore the rest.
        if let Some(c) = v.split_whitespace().find_map(parse_color) {
            out.background = Some(c);
        }
    }
    if let Some(v) = css_prop(&css, "color") {
        out.color = parse_color(&v);
    }
    for prop in ["padding", "margin"] {
        let Some(v) = css_prop(&css, prop) else {
            continue;
        };
        // 1 value = all sides, 2 = (vertical, horizontal), 4 = t r b l.
        let parts: Vec<f32> = v
            .split_whitespace()
            .filter_map(|p| parse_len(p).map(|l| l.px(em)))
            .collect();
        let m = match parts.len() {
            1 => Margin::same(parts[0].round() as i8),
            2 => Margin::symmetric(parts[1].round() as i8, parts[0].round() as i8),
            4 => Margin {
                left: parts[3].round() as i8,
                right: parts[1].round() as i8,
                top: parts[0].round() as i8,
                bottom: parts[2].round() as i8,
            },
            _ => continue,
        };
        if prop == "padding" {
            out.padding = m;
        } else {
            out.margin = m;
        }
    }
    if let Some(v) = css_prop(&css, "border-left") {
        let b = parse_border(&v);
        if !b.none && b.width > 0.0 {
            out.border_left = Some(b);
        }
    }
    if let Some(v) = css_prop(&css, "border") {
        let b = parse_border(&v);
        if !b.none && b.width > 0.0 {
            out.border = Some(b);
        }
    }
    if let Some(v) = css_prop(&css, "width") {
        out.width = parse_len(&v);
    }
    if let Some(v) = css_prop(&css, "text-align") {
        out.align = match v.trim().to_ascii_lowercase().as_str() {
            "center" => Some(Align::Center),
            "right" | "end" => Some(Align::RIGHT),
            _ => None,
        };
    }
    out
}

// ===========================================================================
// A forgiving HTML parser
// ===========================================================================

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Text(String),
    Element(Element),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    /// Raw attribute text, exactly as it appeared between the tag name and `>`.
    pub attrs: String,
    pub children: Vec<Node>,
    /// The whole element, source offsets.
    pub src: std::ops::Range<usize>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<String> {
        attr_value(&self.attrs, name)
    }

    /// Direct children, flattened through wrappers that carry no meaning of
    /// their own (`tbody` inside `table`, `thead`, …).
    pub fn child_elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        self.collect_text(&mut s);
        s
    }

    fn collect_text(&self, out: &mut String) {
        for c in &self.children {
            match c {
                Node::Text(t) => out.push_str(t),
                Node::Element(e) => e.collect_text(out),
            }
        }
    }
}

/// Elements that never have children.
pub fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Tag names that make a document structurally different rather than just
/// styled text. Used to decide whether a fragment is worth rendering as HTML.
pub fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "div"
            | "p"
            | "table"
            | "details"
            | "section"
            | "article"
            | "aside"
            | "blockquote"
            | "figure"
            | "figcaption"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "dd"
            | "pre"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "center"
            | "fieldset"
            | "form"
            | "video"
            | "audio"
            | "iframe"
    )
}

/// Parse a fragment into a forest of nodes.
///
/// Forgiving on purpose: mismatched and unclosed tags are closed at the end
/// rather than rejected, because the input is a Markdown file, not a document
/// that was ever validated. Stray close tags are dropped.
pub fn parse(src: &str) -> Vec<Node> {
    let mut root = Element {
        tag: String::new(),
        attrs: String::new(),
        children: Vec::new(),
        src: 0..src.len(),
    };
    let mut i = 0usize;
    let bytes = src.as_bytes();
    let mut text_start = 0usize;
    // The open elements: their index within their parent, and their tag.
    let mut open: Vec<(usize, String)> = Vec::new();

    while i < src.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // `<!-- comment -->` and `<!doctype>` carry no layout.
        if src[i..].starts_with("<!--") {
            flush_text_node(&mut root, src, text_start, i, &open);
            match src[i..].find("-->") {
                Some(p) => i += p + 3,
                None => i = src.len(),
            }
            text_start = i;
            continue;
        }
        let Some(gt) = src[i..].find('>') else {
            break;
        };
        let gt = i + gt;
        let head = &src[i + 1..gt];
        let closing = head.starts_with('/');
        let body = head.trim_start_matches('/');
        let nlen = body
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(body.len());
        let tag = body[..nlen].to_ascii_lowercase();
        if tag.is_empty() || !tag.starts_with(|c: char| c.is_ascii_alphabetic()) {
            // Not a tag — `<3`, `<https://…>`. Keep it as text.
            i = gt + 1;
            continue;
        }
        let attrs = body[nlen..].to_string();
        flush_text_node(&mut root, src, text_start, i, &open);

        if closing {
            // Close the innermost match; anything opened inside it is unclosed
            // markup and is simply abandoned, which is what browsers do too.
            if let Some(pos) = open.iter().rposition(|(_, t)| *t == tag) {
                let (idx, _) = open[pos];
                open.truncate(pos);
                if let Some(el) = element_at_mut(&mut root, &open, idx) {
                    el.src.end = gt + 1;
                }
            }
        } else if is_void(&tag) {
            let el = Element {
                tag,
                attrs,
                children: Vec::new(),
                src: i..gt + 1,
            };
            push_at(&mut root, &open, Node::Element(el));
        } else {
            let el = Element {
                tag: tag.clone(),
                attrs,
                children: Vec::new(),
                src: i..gt + 1,
            };
            let idx = push_at(&mut root, &open, Node::Element(el));
            open.push((idx, tag));
        }
        i = gt + 1;
        text_start = i;
    }
    flush_text_node(&mut root, src, text_start, src.len(), &open);
    // Unclosed at end of fragment: they end where the fragment does.
    for k in 0..open.len() {
        let (idx, _) = open[k];
        let ancestors = open[..k].to_vec();
        if let Some(el) = element_at_mut(&mut root, &ancestors, idx) {
            el.src.end = src.len();
        }
    }
    root.children
}

/// Emit the pending text run into the innermost open element.
fn flush_text_node(root: &mut Element, src: &str, from: usize, to: usize, open: &[(usize, String)]) {
    if to <= from {
        return;
    }
    let t = src[from..to].to_string();
    if !t.is_empty() {
        push_at(root, open, Node::Text(t));
    }
}

/// Walk to the element that `idx` names inside the element named by `open`.
fn element_at_mut<'a>(
    root: &'a mut Element,
    open: &[(usize, String)],
    idx: usize,
) -> Option<&'a mut Element> {
    let mut cur: &'a mut Element = root;
    for &(k, _) in open {
        // Unreachable in practice: `open` only ever holds element indices.
        cur = match cur.children.get_mut(k) {
            Some(Node::Element(e)) => e,
            _ => return None,
        };
    }
    match cur.children.get_mut(idx) {
        Some(Node::Element(e)) => Some(e),
        _ => None,
    }
}

/// Push a node into the innermost open element, returning its index there.
fn push_at(root: &mut Element, open: &[(usize, String)], node: Node) -> usize {
    // Recursing rather than walking a chain of `&mut` keeps each borrow local,
    // which the borrow checker can follow; a loop that rebinds a `&mut` does
    // not, because the reborrow outlives the rebinding.
    if let Some(&(k, _)) = open.first() {
        if let Some(Node::Element(child)) = root.children.get_mut(k) {
            return push_at(child, &open[1..], node);
        }
        // Unreachable in practice: `open` only ever holds element indices.
    }
    root.children.push(node);
    root.children.len() - 1
}

// ===========================================================================
// Rendering
// ===========================================================================

use crate::fonts;
use crate::math;
use crate::parser::{self, InlineCtx, Leaf};
use crate::theme::Theme;
use std::path::Path;

/// Everything the renderer needs from its caller.
pub struct Ctx<'a> {
    pub theme: &'a Theme,
    /// Link reference definitions, so Markdown inside an HTML block still
    /// resolves `[ref]` links.
    pub defs: &'a std::collections::HashMap<String, parser::LinkDef>,
    pub doc_dir: Option<&'a Path>,
    /// Base font size and line height multiple.
    pub size: f32,
    pub line_height: f32,
    /// Default text colour.
    pub color: Color32,
}

/// The style an inline run inherits from the HTML elements around it.
#[derive(Clone, Debug)]
struct Fmt {
    color: Option<Color32>,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    highlight: bool,
    scale: f32,
    link: Option<String>,
}

impl Default for Fmt {
    fn default() -> Self {
        Self {
            color: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            code: false,
            highlight: false,
            scale: 1.0,
            link: None,
        }
    }
}

/// One piece of an inline run, before it is laid out.
#[derive(Clone)]
enum Inline {
    Text(String, Fmt),
    Image {
        url: String,
        alt: String,
        width: Option<f32>,
    },
    Math(String, bool),
    Break,
}

/// Lay `src` out as HTML and return the height it took.
///
/// `None` means "there is no block-level HTML here", which sends the caller
/// back to rendering the block as an ordinary paragraph — where the inline
/// handling in `parser.rs` already deals with `<b>` and friends.
pub fn draw(ui: &mut Ui, ctx: &Ctx, src: &str) -> Option<f32> {
    let nodes = parse(src);
    let structural = nodes.iter().any(|n| match n {
        Node::Element(e) => is_block_tag(&e.tag) || is_void(&e.tag),
        Node::Text(_) => false,
    });
    if !structural || !has_content(&nodes) {
        return None;
    }
    let inner = ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = ctx.size * 0.34;
        render_nodes(ui, ctx, &nodes, &Fmt::default());
    });
    Some(inner.response.rect.height().max(ctx.size))
}

/// Does this fragment have anything to show at all?
///
/// `<div></div>` and friends would otherwise claim a block and draw nothing.
fn has_content(nodes: &[Node]) -> bool {
    nodes.iter().any(|n| match n {
        Node::Text(t) => !t.trim().is_empty(),
        Node::Element(e) => {
            matches!(e.tag.as_str(), "img" | "hr" | "td" | "th") || has_content(&e.children)
        }
    })
}

/// Elements that break the flow rather than joining it.
fn breaks_flow(tag: &str) -> bool {
    is_block_tag(tag) || matches!(tag, "td" | "th" | "tr" | "thead" | "tbody" | "tfoot")
}

/// Collapse every run of whitespace to one space, the way HTML does.
///
/// Leading and trailing spaces are kept: they are significant when the text
/// node sits next to an inline element (`a <b>x</b>` must not become `ax`).
fn collapse_ws(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut in_ws = false;
    for c in t.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

fn render_nodes(ui: &mut Ui, ctx: &Ctx, nodes: &[Node], fmt: &Fmt) {
    let mut run: Vec<Inline> = Vec::new();
    for n in nodes {
        match n {
            Node::Text(t) => {
                // HTML collapses whitespace, and the indentation in a
                // pretty-printed block is worth a full blank line per element if
                // it is not collapsed. `<pre>` bypasses this.
                let collapsed = collapse_ws(t);
                // A whitespace-only node at a block boundary is indentation,
                // not a line of text.
                if collapsed.trim().is_empty() && run.is_empty() {
                    continue;
                }
                run.push(Inline::Text(collapsed, fmt.clone()));
            }
            Node::Element(e) if breaks_flow(&e.tag) => {
                flush(ui, ctx, &mut run);
                render_element(ui, ctx, e, fmt);
            }
            Node::Element(e) => inline_element(e, fmt, &mut run),
        }
    }
    flush(ui, ctx, &mut run);
}

fn render_element(ui: &mut Ui, ctx: &Ctx, e: &Element, fmt: &Fmt) {
    match e.tag.as_str() {
        "table" => render_table(ui, ctx, e, fmt),
        "details" => render_details(ui, ctx, e, fmt),
        "hr" => {
            let (rect, _) = ui.allocate_exact_size(
                vec2(ui.available_width(), ctx.size * 0.9),
                Sense::hover(),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rect.center().y),
                    egui::pos2(rect.right(), rect.center().y),
                ],
                Stroke::new(1.0_f32, ctx.theme.border),
            );
        }
        "img" => {
            // A block-level image gets its own line, honouring `width`.
            let mut run = vec![Inline::Image {
                url: e.attr("src").unwrap_or_default(),
                alt: e.attr("alt").unwrap_or_default(),
                width: e.attr("width")
                    .and_then(|w| w.trim_end_matches("px").parse().ok()),
            }];
            flush(ui, ctx, &mut run);
        }
        "br" => {}
        "pre" => render_pre(ui, ctx, e),
        "ul" | "ol" => render_list(ui, ctx, e, fmt),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = e.tag.as_bytes()[1] - b'0';
            let mut f = fmt.clone();
            f.bold = true;
            f.scale *= match level {
                1 => 1.9,
                2 => 1.5,
                3 => 1.26,
                4 => 1.12,
                5 => 1.0,
                _ => 0.92,
            };
            if f.color.is_none() {
                f.color = Some(ctx.theme.heading);
            }
            render_nodes(ui, ctx, &e.children, &f);
        }
        _ => render_container(ui, ctx, e, fmt),
    }
}

/// A box: background, padding, border. Everything the CSS subset can express.
fn render_container(ui: &mut Ui, ctx: &Ctx, e: &Element, fmt: &Fmt) {
    let st = box_style(&e.attrs, ctx.size);
    let mut f = fmt.clone();
    if st.color.is_some() {
        f.color = st.color;
    }
    let avail = ui.available_width();
    let inner_w = st
        .width
        .map(|l| match l {
            Len::Pct(p) => avail * p * 0.01,
            other => other.px(ctx.size),
        })
        .unwrap_or(avail)
        .min(avail)
        - st.padding.left as f32
        - st.padding.right as f32;

    let decorations = st.background.is_some() || st.border.is_some() || st.border_left.is_some();
    let mut frame = Frame::NONE
        .inner_margin(st.padding)
        .outer_margin(st.margin);
    if let Some(bg) = st.background {
        frame = frame.fill(bg);
    }
    if let Some(b) = st.border {
        frame = frame.stroke(Stroke::new(
            b.width,
            b.color.unwrap_or(ctx.theme.border),
        ));
    }
    if decorations {
        frame = frame.corner_radius(CornerRadius::same(4));
    }

    // `padding: 0` on a box with a left rule still needs room, or the rule
    // touches the text.
    let left_bar = st.border_left;
    let inner = frame.show(ui, |ui| {
        // A block element is as wide as its container, like a browser makes it.
        // Without the minimum, the box would shrink-wrap its text and a tinted
        // callout would end mid-sentence.
        let w = inner_w.max(40.0);
        ui.set_min_width(w);
        ui.set_max_width(w);
        let pad = if left_bar.is_some() { 10.0 } else { 0.0 };
        ui.horizontal(|ui| {
            if pad > 0.0 {
                ui.add_space(pad);
            }
            ui.vertical(|ui| {
                ui.set_max_width((inner_w - pad).max(40.0));
                render_nodes(ui, ctx, &e.children, &f);
            });
        });
    });

    if let Some(b) = left_bar {
        let r = inner.response.rect;
        let w = b.width.max(1.0).min(r.width());
        ui.painter().rect_filled(
            Rect::from_min_size(r.left_top(), vec2(w, r.height())),
            CornerRadius::same(2),
            b.color.unwrap_or(ctx.theme.accent),
        );
    }
}

fn render_pre(ui: &mut Ui, ctx: &Ctx, e: &Element) {
    let text = e.text();
    let text = text.trim_start_matches('\n').trim_end();
    let galley = ui.fonts(|f| {
        f.layout(
            text.to_string(),
            egui::FontId::new(
                ctx.size * 0.9,
                fonts::mono_family_for(false),
            ),
            ctx.theme.code_text,
            ui.available_width().max(64.0) - 20.0,
        )
    });
    Frame::NONE
        .fill(ctx.theme.code_bg)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.add(egui::Label::new(galley));
        });
}

fn render_list(ui: &mut Ui, ctx: &Ctx, e: &Element, fmt: &Fmt) {
    let ordered = e.tag == "ol";
    let start: usize = e
        .attr("start")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1);
    let mut n = start;
    for li in e.child_elements().filter(|c| c.tag == "li") {
        ui.horizontal_top(|ui| {
            let marker = if ordered {
                let m = format!("{n}.");
                n += 1;
                m
            } else {
                "\u{2022}".to_string()
            };
            let g = ui.fonts(|f| {
                f.layout_no_wrap(
                    marker,
                    egui::FontId::new(ctx.size, fonts::family_for(false, false)),
                    ctx.color,
                )
            });
            let w = g.rect.width();
            ui.add(egui::Label::new(g));
            ui.add_space((ctx.size * 0.7 - w).max(4.0));
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 2.0).max(48.0));
                render_nodes(ui, ctx, &li.children, fmt);
            });
        });
    }
}

fn render_details(ui: &mut Ui, ctx: &Ctx, e: &Element, fmt: &Fmt) {
    let summary = e
        .child_elements()
        .find(|c| c.tag == "summary")
        .map(|s| s.text())
        .unwrap_or_else(|| "详情".to_string());
    // Open by default: a Markdown viewer that hides content behind a click is
    // hiding the document the reader came to read.
    let id = ui.id().with(("html-details", e.src.start));
    let mut open = ui
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or(id, true));
    let mut f = fmt.clone();
    f.bold = true;

    ui.horizontal(|ui| {
        let tri = if open { "\u{25be}" } else { "\u{25b8}" };
        let g = ui.fonts(|ff| {
            ff.layout_no_wrap(
                tri.to_string(),
                egui::FontId::new(ctx.size * 0.9, fonts::family_for(false, false)),
                ctx.theme.text_muted,
            )
        });
        let r = ui
            .add(egui::Label::new(g).sense(Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if r.clicked() {
            open = !open;
            ui.ctx().data_mut(|d| d.insert_temp(id, open));
        }
        let body = summary.clone();
        let sg = ui.fonts(|ff| {
            ff.layout(
                body,
                egui::FontId::new(ctx.size, fonts::family_for(true, false)),
                ctx.color,
                ui.available_width() - 4.0,
            )
        });
        let sr = ui
            .add(egui::Label::new(sg).sense(Sense::click()))
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        if sr.clicked() {
            open = !open;
            ui.ctx().data_mut(|d| d.insert_temp(id, open));
        }
    });
    let _ = f;

    if open {
        for child in &e.children {
            match child {
                Node::Element(c) if c.tag == "summary" => {}
                _ => render_nodes(ui, ctx, std::slice::from_ref(child), fmt),
            }
        }
    }
}

fn render_table(ui: &mut Ui, ctx: &Ctx, e: &Element, fmt: &Fmt) {
    let size = ctx.size * 0.94;
    let line_h = size * ctx.line_height;
    let pad_x = 10.0;
    let pad_y = 7.0;

    // Flatten to rows, running through `thead`/`tbody` wrappers.
    let mut rows: Vec<(bool, Vec<String>)> = Vec::new();
    fn walk(e: &Element, header: bool, rows: &mut Vec<(bool, Vec<String>)>) {
        for c in e.child_elements() {
            match c.tag.as_str() {
                "tr" => {
                    let head = header || rows.is_empty();
                    let cells = c
                        .child_elements()
                        .filter(|x| x.tag == "td" || x.tag == "th")
                        .map(|x| x.text().trim().to_string())
                        .collect();
                    rows.push((head, cells));
                }
                "thead" => walk(c, true, rows),
                "tbody" | "tfoot" => walk(c, false, rows),
                _ => walk(c, header, rows),
            }
        }
    }
    walk(e, false, &mut rows);
    let ncol = rows.iter().map(|(_, c)| c.len()).max().unwrap_or(0);
    if rows.is_empty() || ncol == 0 {
        return;
    }

    let avail = ui.available_width().max(80.0);
    let mut col_w = vec![0.0f32; ncol];
    for (_, cells) in &rows {
        for (c, t) in cells.iter().enumerate() {
            let g = ui.fonts(|f| {
                f.layout_no_wrap(
                    t.clone(),
                    egui::FontId::new(size, fonts::family_for(false, false)),
                    ctx.color,
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
        *w = w.max(48.0);
    }
    let total_w: f32 = col_w.iter().sum::<f32>() + pad_x * 2.0 * ncol as f32;

    // Measure first, then paint, so the border can be drawn under the cells.
    let left = ui.cursor().left();
    let top = ui.cursor().top();
    let mut y = top;
    let mut laid: Vec<(f32, f32, Vec<(f32, f32, std::sync::Arc<egui::Galley>)>)> = Vec::new();
    for (head, cells) in &rows {
        let mut row_h = line_h + pad_y * 2.0;
        let mut placed = Vec::new();
        let mut x = left;
        for (c, t) in cells.iter().enumerate() {
            let cw = *col_w.get(c).unwrap_or(&48.0);
            let g = ui.fonts(|f| {
                f.layout(
                    t.clone(),
                    egui::FontId::new(
                        size,
                        fonts::family_for(*head, false),
                    ),
                    if *head { ctx.theme.heading } else { ctx.color },
                    cw,
                )
            });
            row_h = row_h.max(g.rect.height() + pad_y * 2.0);
            placed.push((x, cw, g));
            x += cw + pad_x * 2.0;
        }
        laid.push((y, row_h, placed));
        y += row_h;
    }
    let outer = Rect::from_min_size(
        egui::pos2(left, top),
        vec2(total_w.min(avail).max(64.0), y - top),
    );
    ui.allocate_exact_size(vec2(avail, y - top), Sense::hover());

    ui.painter().rect_stroke(
        outer,
        CornerRadius::same(5),
        Stroke::new(1.0_f32, ctx.theme.border),
        egui::StrokeKind::Inside,
    );
    for (r, (ry, rh, placed)) in laid.iter().enumerate() {
        if rows[r].0 {
            ui.painter().rect_filled(
                Rect::from_min_size(egui::pos2(outer.left(), *ry), vec2(outer.width(), *rh)),
                0.0,
                ctx.theme.table_head_bg,
            );
        }
        if r > 0 {
            ui.painter().line_segment(
                [
                    egui::pos2(outer.left(), *ry),
                    egui::pos2(outer.left() + outer.width(), *ry),
                ],
                Stroke::new(1.0_f32, ctx.theme.border),
            );
        }
        for (cx, cw, g) in placed {
            let h = rows[r].0;
            let color = if h { ctx.theme.heading } else { ctx.color };
            let pos = egui::pos2(*cx + pad_x, ry + (rh - g.rect.height()) * 0.5);
            ui.painter().galley(pos, g.clone(), color);
            let _ = cw;
        }
    }
    let _ = fmt;
}

// ---------------------------------------------------------------------------
// Inline
// ---------------------------------------------------------------------------

/// Fold an inline element into the pending run, widening the style.
fn inline_element(e: &Element, fmt: &Fmt, out: &mut Vec<Inline>) {
    let mut f = fmt.clone();
    match e.tag.as_str() {
        "br" => {
            out.push(Inline::Break);
            return;
        }
        "wbr" => return,
        "img" => {
            out.push(Inline::Image {
                url: e.attr("src").unwrap_or_default(),
                alt: e.attr("alt").unwrap_or_default(),
                width: e
                    .attr("width")
                    .and_then(|w| w.trim_end_matches("px").trim().parse().ok()),
            });
            return;
        }
        "b" | "strong" => f.bold = true,
        "i" | "em" | "cite" | "var" | "dfn" => f.italic = true,
        "u" | "ins" => f.underline = true,
        "s" | "del" | "strike" => f.strike = true,
        "code" | "kbd" | "samp" | "tt" => f.code = true,
        "mark" => f.highlight = true,
        "small" => f.scale *= 0.86,
        "sup" | "sub" => f.scale *= 0.78,
        "big" => f.scale *= 1.15,
        "a" => {
            f.link = e.attr("href");
            f.underline = true;
        }
        "span" | "font" => {
            let from_style = e
                .attr("style")
                .and_then(|s| css_prop(&s, "color"))
                .and_then(|v| parse_color(&v));
            let from_attr = e.attr("color").and_then(|v| parse_color(&v));
            if let Some(c) = from_style.or(from_attr) {
                f.color = Some(c);
            }
        }
        _ => {}
    }
    for c in &e.children {
        match c {
            Node::Text(t) => out.push(Inline::Text(t.clone(), f.clone())),
            Node::Element(ch) => inline_element(ch, &f, out),
        }
    }
}

/// A laid-out inline atom: text to shape, or something with its own geometry.
enum Piece {
    Text(String, egui::TextFormat),
    Media(Inline),
    Break,
}

/// Turn the pending run into pieces, expanding Markdown inside the text.
///
/// Markdown stays live inside an HTML block, which is how people write a
/// `<div>` callout whose body has `**bold**` and `$x^2$` in it.
fn pieces(ctx: &Ctx, run: &[Inline]) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let icon = InlineCtx {
        defs: ctx.defs,
        keep_markers: false,
    };
    for item in run {
        match item {
            Inline::Text(t, f) => {
                for l in parser::parse_inline(t, &icon) {
                    match l {
                        Leaf::Span {
                            text, style, link, ..
                        } => {
                            let mut g = f.clone();
                            if link.is_some() {
                                g.link = link.clone();
                            }
                            out.push(Piece::Text(text, text_format(ctx, &g, style)));
                        }
                        Leaf::Break { .. } => out.push(Piece::Break),
                        Leaf::Math { tex, display, .. } => {
                            out.push(Piece::Media(Inline::Math(tex, display)))
                        }
                        Leaf::Image { url, alt, .. } => out.push(Piece::Media(Inline::Image {
                            url,
                            alt,
                            width: None,
                        })),
                        Leaf::Footnote { name, .. } => {
                            let mut g = f.clone();
                            g.scale *= 0.72;
                            out.push(Piece::Text(
                                format!("[{name}]"),
                                text_format(ctx, &g, parser::Style::default()),
                            ));
                        }
                    }
                }
            }
            Inline::Break => out.push(Piece::Break),
            other => out.push(Piece::Media(other.clone())),
        }
    }
    out
}

fn text_format(ctx: &Ctx, base: &Fmt, style: parser::Style) -> egui::TextFormat {
    let bold = base.bold || style.strong;
    let italic = base.italic || style.em;
    let code = base.code || style.code;
    let size = ctx.size * base.scale * if code { 0.92 } else { 1.0 };
    let family = if code {
        fonts::mono_family_for(bold)
    } else {
        fonts::family_for(bold, italic)
    };
    // An explicit colour from the author wins; otherwise `<code>` gets the code
    // colour and everything else the block's text colour.
    let mut color = base
        .color
        .or(style.color.map(from_packed))
        .unwrap_or(if code {
            ctx.theme.inline_code_text
        } else {
            ctx.color
        });
    let mut bg = if code {
        ctx.theme.inline_code_bg
    } else if base.highlight || style.highlight {
        ctx.theme.highlight_bg
    } else {
        Color32::TRANSPARENT
    };
    let is_link = base.link.is_some();
    if is_link {
        color = ctx.theme.link;
        bg = Color32::TRANSPARENT;
    }
    egui::TextFormat {
        font_id: egui::FontId::new(size, family),
        color,
        background: bg,
        italics: italic,
        underline: if base.underline || is_link {
            Stroke::new(1.0_f32, color)
        } else {
            Stroke::NONE
        },
        strikethrough: if base.strike || style.strike {
            Stroke::new(1.2_f32, color)
        } else {
            Stroke::NONE
        },
        line_height: Some(size * ctx.line_height),
        ..Default::default()
    }
}

fn from_packed(c: u32) -> Color32 {
    Color32::from_rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

/// Lay out one chunk of text, then let the caller continue on a new row.
fn flush_text(ui: &mut Ui, job: &mut egui::text::LayoutJob) {
    if job.text.is_empty() {
        return;
    }
    let mut j = std::mem::take(job);
    j.wrap.max_width = ui.available_width().max(32.0);
    let galley = ui.fonts(|f| f.layout_job(j));
    ui.add(egui::Label::new(galley).sense(Sense::click()));
}

fn flush(ui: &mut Ui, ctx: &Ctx, run: &mut Vec<Inline>) {
    if run.is_empty() {
        return;
    }
    let items = pieces(ctx, run);
    run.clear();
    if items.is_empty() {
        return;
    }
    let avail = ui.available_width().max(32.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.set_max_width(avail);
        let mut job = egui::text::LayoutJob::default();
        for p in &items {
            match p {
                Piece::Text(t, fmt) => {
                    let start = job.text.len();
                    job.text.push_str(t);
                    job.sections.push(egui::text::LayoutSection {
                        leading_space: 0.0,
                        byte_range: start..job.text.len(),
                        format: fmt.clone(),
                    });
                }
                Piece::Break => {
                    flush_text(ui, &mut job);
                    ui.end_row();
                }
                Piece::Media(m) => {
                    flush_text(ui, &mut job);
                    media(ui, ctx, m);
                }
            }
        }
        flush_text(ui, &mut job);
    });
}

/// An image or a formula sitting in the text flow.
fn media(ui: &mut Ui, ctx: &Ctx, item: &Inline) {
    match item {
        Inline::Image { url, alt, width } => {
            if url.is_empty() {
                return;
            }
            let avail = ui.available_width();
            let uri = crate::render::resolve_image(ctx.doc_dir, url);
            let mut img = egui::Image::from_uri(uri)
                .maintain_aspect_ratio(true)
                .sense(Sense::click());
            let max_w = width.unwrap_or(avail);
            match crate::render::image_dimensions(ctx.doc_dir, url) {
                Some((w, h)) if w > 0.0 => {
                    let scale = (max_w / w).min(1.0);
                    img = img.fit_to_exact_size(vec2(w * scale, h * scale));
                }
                _ => {
                    img = img.max_width(max_w.max(24.0).min(avail.max(24.0)));
                }
            }
            ui.add(img);
            let _ = alt;
        }
        Inline::Math(tex, display) => {
            let size = if *display {
                ctx.size * 1.15
            } else {
                ctx.size * 1.02
            };
            let mb = math::layout(ui, tex, size, ctx.color, *display);
            let sz = vec2(mb.width.max(4.0), mb.height().max(size));
            let (rect, _) = ui.allocate_exact_size(sz, Sense::click());
            math::paint(
                ui.painter(),
                egui::pos2(
                    rect.left(),
                    rect.top() + (rect.height() - mb.height()) * 0.5 + mb.ascent,
                ),
                &mb,
                ctx.color,
            );
        }
        Inline::Text(..) | Inline::Break => {}
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn headless() -> egui::Context {
        let ctx = egui::Context::default();
        crate::fonts::install(&ctx);
        Theme::light().apply(&ctx);
        ctx
    }

    fn ctx_for<'a>(
        theme: &'a Theme,
        defs: &'a std::collections::HashMap<String, parser::LinkDef>,
    ) -> Ctx<'a> {
        Ctx {
            theme,
            defs,
            doc_dir: None,
            size: 16.0,
            line_height: 1.7,
            color: theme.text,
        }
    }

    /// Render `src` headlessly and report the height, every solid fill, and the
    /// filled rectangles. Measuring the *painted shapes* rather than the return
    /// value is the point: a card that returns the right height but paints
    /// nothing is still broken.
    fn paint(src: &str) -> (Option<f32>, Vec<Color32>) {
        use egui::epaint::Shape;
        let ctx = headless();
        let theme = Theme::light();
        let defs = std::collections::HashMap::new();
        let mut height = None;
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 700.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let c = ctx_for(&theme, &defs);
                    height = draw(ui, &c, src);
                });
            },
        );
        let mut fills = Vec::new();
        for cs in &out.shapes {
            if let Shape::Rect(r) = &cs.shape {
                if r.fill != Color32::TRANSPARENT {
                    fills.push(r.fill);
                }
            }
        }
        (height, fills)
    }

    #[test]
    fn attributes_are_read_quoted_or_bare() {
        let a = r##" src="x.png" alt='a picture' width=120 data-x="#fff" "##;
        assert_eq!(attr_value(a, "src").as_deref(), Some("x.png"));
        assert_eq!(attr_value(a, "alt").as_deref(), Some("a picture"));
        assert_eq!(attr_value(a, "width").as_deref(), Some("120"));
        assert_eq!(attr_value(a, "data-x").as_deref(), Some("#fff"));
        assert_eq!(attr_value(a, "missing"), None);
    }

    #[test]
    fn the_css_subset_is_understood() {
        assert_eq!(parse_color("#fff"), Some(Color32::from_rgb(255, 255, 255)));
        assert_eq!(parse_color("#4c8bf5"), Some(Color32::from_rgb(0x4c, 0x8b, 0xf5)));
        assert_eq!(parse_color("rgb(1, 2, 3)"), Some(Color32::from_rgb(1, 2, 3)));
        assert_eq!(parse_color("not-a-colour"), None);

        let css = "padding: 12px; border-left: 4px solid #4c8bf5; background: #f4f7fb;";
        assert_eq!(css_prop(css, "padding").as_deref(), Some("12px"));
        assert_eq!(css_prop(css, "border-left").as_deref(), Some("4px solid #4c8bf5"));

        let st = box_style(r#" style="padding: 12px; background: #f4f7fb;" "#, 16.0);
        assert_eq!(st.padding, egui::Margin::same(12));
        assert_eq!(st.background, Some(Color32::from_rgb(0xf4, 0xf7, 0xfb)));
    }

    #[test]
    fn a_border_shorthand_reads_in_any_order() {
        let b = parse_border("4px solid #4c8bf5");
        assert_eq!(b.width, 4.0);
        assert_eq!(b.color, Some(Color32::from_rgb(0x4c, 0x8b, 0xf5)));
        let b = parse_border("solid red");
        assert!(b.width > 0.0, "a style keyword implies a width");
        let b = parse_border("none");
        assert!(b.none && b.width == 0.0);
    }

    #[test]
    fn a_collapsed_element_holds_its_children() {
        // Unclosed markup is common in hand-written Markdown, so it has to nest
        // rather than be discarded.
        let nodes = parse("<div><b>x");
        let Node::Element(div) = &nodes[0] else {
            panic!("expected a div");
        };
        assert_eq!(div.tag, "div");
        assert_eq!(div.children.len(), 1);
        assert_eq!(div.text(), "x");
    }

    #[test]
    fn a_comment_is_not_text() {
        let nodes = parse("<div>a<!-- hidden -->b</div>");
        let Node::Element(div) = &nodes[0] else {
            panic!("expected a div");
        };
        assert_eq!(div.text(), "ab");
    }

    #[test]
    fn a_fragment_without_structure_falls_back_to_a_paragraph() {
        // No block element: the caller renders it as Markdown, where inline
        // handling deals with `<b>`.
        assert!(paint("just some words").0.is_none());
        assert!(paint("<b>bold</b> and text").0.is_none());
        assert!(paint("<span style=\"color:red\">x</span>").0.is_none());
        assert!(paint("<div></div>").0.is_none(), "empty markup is not a block");
        // 3.8 in the stress document: an element we do not know. It stays a
        // paragraph, where the tag is shown as code rather than dropped.
        assert!(paint("<widget>hello</widget>").0.is_none());
    }

    /// Widths of the painted rectangles, for asserting on box geometry.
    fn painted_rects(src: &str) -> Vec<(Color32, f32)> {
        use egui::epaint::Shape;
        let ctx = headless();
        let theme = Theme::light();
        let defs = std::collections::HashMap::new();
        let out = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 700.0))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let c = ctx_for(&theme, &defs);
                    draw(ui, &c, src);
                });
            },
        );
        let mut v = Vec::new();
        for cs in &out.shapes {
            if let Shape::Rect(r) = &cs.shape {
                if r.fill != Color32::TRANSPARENT {
                    v.push((r.fill, r.rect.width()));
                }
            }
        }
        v
    }

    #[test]
    fn a_block_box_spans_its_container() {
        // A browser stretches a `<div>` to the full column width; shrink-wrapping
        // it would make a tinted callout stop mid-sentence.
        let rects = painted_rects("<div style=\"background:#fff8e1;\">short</div>");
        let (_, w) = rects
            .iter()
            .find(|(c, _)| *c == Color32::from_rgb(0xff, 0xf8, 0xe1))
            .expect("the background");
        assert!(*w > 800.0, "the box is only {w}px wide");
    }

    #[test]
    fn a_styled_div_paints_its_background() {
        let (h, fills) = paint(
            r#"<div style="padding: 12px; border-left: 4px solid #4c8bf5; background: #f4f7fb;">
  <strong>块级 HTML：</strong>一个提示框。
</div>"#,
        );
        let h = h.expect("a div is a block");
        let bg = Color32::from_rgb(0xf4, 0xf7, 0xfb);
        let bar = Color32::from_rgb(0x4c, 0x8b, 0xf5);
        assert!(fills.contains(&bg), "no background painted: {fills:?}");
        assert!(fills.contains(&bar), "no left rule painted: {fills:?}");
        // One line of text plus the 12px padding on each side.
        assert!(h > 40.0 && h < 60.0, "height {h} does not look like one line");
    }

    #[test]
    fn a_table_gets_a_row_per_row_element() {
        let (h, _) = paint(
            "<table><thead><tr><th>a</th><th>b</th></tr></thead>\
             <tbody><tr><td>1</td><td>2</td></tr><tr><td>3</td><td>4</td></tr></tbody></table>",
        );
        let h = h.expect("a table is a block");
        // Three rows, plus the padding each cell carries.
        assert!(h > 100.0, "height {h} is too short for three rows");
    }

    #[test]
    fn markdown_still_applies_inside_an_html_block() {
        // 3.6 in the stress document: a `<div>` whose body has `**bold**`.
        let (h, fills) = paint("<div style=\"background:#fff8e1;\">**bold** and $x^2$</div>");
        assert!(h.is_some(), "should lay out");
        assert!(
            fills.contains(&Color32::from_rgb(0xff, 0xf8, 0xe1)),
            "the div lost its background: {fills:?}"
        );
    }

    #[test]
    fn whitespace_between_elements_collapses() {
        assert_eq!(collapse_ws("a \n\t b"), "a b");
        assert_eq!(collapse_ws("  "), " ");
        // Trailing space is kept so that `a <b>x</b>` does not become `ax`.
        assert_eq!(collapse_ws("a "), "a ");
    }

    #[test]
    fn details_keeps_its_summary_and_body() {
        let (h, _) = paint(
            "<details>\n  <summary>点开查看</summary>\n  <p>折叠的内容。</p>\n</details>",
        );
        let h = h.expect("details is a block");
        // Summary line plus the body, which is open by default.
        assert!(h > 45.0, "height {h}: the body is not being shown");
    }

    #[test]
    fn a_list_gets_a_bullet_per_item() {
        let (h, _) = paint("<ul><li>one</li><li>two</li><li>three</li></ul>");
        let h = h.expect("a list is a block");
        assert!(h > 60.0, "height {h} is too short for three items");
    }
}
