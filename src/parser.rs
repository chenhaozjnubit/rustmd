//! Markdown parsing.
//!
//! Two independent layers:
//!
//! * **Block parsing** (`parse`) turns the raw source into a flat, gapless list of
//!   [`Block`]s. Every byte of the document belongs to exactly one block
//!   (including the newline that terminates it), which is what makes the
//!   round-trip safe: editing a block rewrites a byte range of the original text
//!   and nothing else. Each block also carries a *content* string with the
//!   structural markers (`>`, `- `, `#`, fences) stripped, plus a segment map so
//!   that a click on rendered content can be translated back to a source offset.
//!
//! * **Inline parsing** (`parse_inline`) turns a block's content into a flat list
//!   of styled leaves. It implements the CommonMark delimiter-run algorithm for
//!   emphasis so that intraword underscores, `***bold italic***` and
//!   `**a *b* c**` all behave.
//!
//! Neither layer allocates per-frame state: a `Parsed` is rebuilt whenever the
//! text changes, which for editor-sized documents is well under a millisecond.

use std::collections::HashMap;
use std::ops::Range;

// ===========================================================================
// Lines
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// Byte offset of the first character.
    pub start: usize,
    /// Byte offset one past the last character, excluding the newline.
    pub end: usize,
}

/// Split into lines. An empty document yields a single empty line so that the
/// editor always has somewhere to put the caret.
pub fn split_lines(text: &str) -> Vec<Line> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let s = i;
        while i < b.len() && b[i] != b'\n' {
            i += 1;
        }
        let mut e = i;
        if e > s && b[e - 1] == b'\r' {
            e -= 1;
        }
        out.push(Line { start: s, end: e });
        if i < b.len() {
            i += 1;
        }
    }
    if out.is_empty() {
        out.push(Line { start: 0, end: 0 });
    }
    out
}

fn line_full_end(text: &str, line: Line) -> usize {
    if text.as_bytes().get(line.end) == Some(&b'\n') {
        line.end + 1
    } else {
        line.end
    }
}

// ===========================================================================
// Small lexical helpers (all operate on a single quote-stripped line)
// ===========================================================================

fn is_blank(s: &str) -> bool {
    s.chars().all(|c| c == ' ' || c == '\t')
}

/// (visual columns, bytes consumed) of the leading whitespace.
fn leading_ws(s: &str) -> (usize, usize) {
    let mut cols = 0;
    let mut bytes = 0;
    for c in s.chars() {
        match c {
            ' ' => {
                cols += 1;
                bytes += 1;
            }
            '\t' => {
                cols += 4 - (cols % 4);
                bytes += 1;
            }
            _ => break,
        }
    }
    (cols, bytes)
}

/// Strip a run of `>` blockquote markers. Returns (depth, bytes consumed).
fn strip_quote(s: &str) -> (usize, usize) {
    let mut depth = 0;
    let mut pos = 0;
    loop {
        let rest = &s[pos..];
        let (cols, ws) = leading_ws(rest);
        if cols > 3 {
            break;
        }
        let after = &rest[ws..];
        if after.starts_with('>') {
            let mut adv = ws + 1;
            if after[1..].starts_with(' ') {
                adv += 1;
            }
            pos += adv;
            depth += 1;
        } else {
            break;
        }
    }
    (depth, pos)
}

/// ATX heading. Returns (level, bytes consumed through the required space).
fn heading_marker(s: &str) -> Option<(u8, usize)> {
    let (_, ws) = leading_ws(s);
    let t = &s[ws..];
    let hashes = t.bytes().take_while(|&c| c == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &t[hashes..];
    if after.is_empty() {
        return Some((hashes as u8, ws + hashes));
    }
    if after.starts_with(' ') || after.starts_with('\t') {
        let extra = after.len() - after.trim_start_matches([' ', '\t']).len();
        return Some((hashes as u8, ws + hashes + extra));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fence {
    pub ch: u8,
    pub len: usize,
    pub indent_cols: usize,
    pub indent_bytes: usize,
}

/// Opening or closing code fence.
fn fence_marker(s: &str) -> Option<Fence> {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return None;
    }
    let t = &s[ws..];
    let ch = *t.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = t.bytes().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    // Info string of a backtick fence may not contain backticks.
    if ch == b'`' && t[len..].contains('`') {
        return None;
    }
    Some(Fence {
        ch,
        len,
        indent_cols: cols,
        indent_bytes: ws,
    })
}

fn fence_info(s: &str, f: Fence) -> String {
    s[f.indent_bytes + f.len..].trim().to_string()
}

/// Thematic break: `---`, `***`, `___` (3+, spaces allowed between).
fn is_rule(s: &str) -> bool {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return false;
    }
    let t = s[ws..].trim_end();
    let mut ch = None;
    let mut count = 0;
    for c in t.chars() {
        if c == ' ' || c == '\t' {
            continue;
        }
        match ch {
            None => {
                if c == '-' || c == '*' || c == '_' {
                    ch = Some(c);
                    count = 1;
                } else {
                    return false;
                }
            }
            Some(p) => {
                if c == p {
                    count += 1;
                } else {
                    return false;
                }
            }
        }
    }
    count >= 3
}

/// Setext underline (`===` / `---`). `---` is only a setext underline when a
/// paragraph is open; lone `-` runs of 3+ are handled by `is_rule` first.
fn setext_level(s: &str) -> Option<u8> {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return None;
    }
    let t = s[ws..].trim_end();
    if t.is_empty() {
        return None;
    }
    let c = t.as_bytes()[0];
    if c != b'=' && c != b'-' {
        return None;
    }
    if t.bytes().all(|b| b == c) {
        Some(if c == b'=' { 1 } else { 2 })
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bullet {
    pub cols: usize,
    pub bytes: usize,
    pub marker: u8,
    /// Bytes from line start through the marker and its trailing space.
    pub content_off: usize,
}

fn bullet_marker(s: &str) -> Option<Bullet> {
    let (cols, ws) = leading_ws(s);
    if cols > 8 {
        return None;
    }
    let t = &s[ws..];
    let m = *t.as_bytes().first()?;
    if m != b'-' && m != b'+' && m != b'*' {
        return None;
    }
    let rest = &t[1..];
    let adv = if rest.is_empty() {
        0
    } else if rest.starts_with(' ') || rest.starts_with('\t') {
        rest.len() - rest.trim_start_matches([' ', '\t']).len()
    } else if rest.starts_with('\n') {
        0
    } else {
        return None;
    };
    // `- - -` is a thematic break, not a list.
    if adv == 0 && !rest.is_empty() {
        return None;
    }
    Some(Bullet {
        cols,
        bytes: ws,
        marker: m,
        content_off: ws + 1 + adv,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ordered {
    pub cols: usize,
    pub bytes: usize,
    pub number: u64,
    pub delim: u8,
    pub content_off: usize,
}

fn ordered_marker(s: &str) -> Option<Ordered> {
    let (cols, ws) = leading_ws(s);
    if cols > 8 {
        return None;
    }
    let t = &s[ws..];
    let digits = t.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let delim = *t.as_bytes().get(digits)?;
    if delim != b'.' && delim != b')' {
        return None;
    }
    let rest = &t[digits + 1..];
    let adv = if rest.is_empty() {
        0
    } else if rest.starts_with(' ') || rest.starts_with('\t') {
        rest.len() - rest.trim_start_matches([' ', '\t']).len()
    } else {
        return None;
    };
    Some(Ordered {
        cols,
        bytes: ws,
        number: t[..digits].parse().unwrap_or(1),
        delim,
        content_off: ws + digits + 1 + adv,
    })
}

/// Task list checkbox at the start of a list item's content.
fn task_marker(s: &str) -> Option<(bool, usize)> {
    let b = s.as_bytes();
    if b.len() < 3 || b[0] != b'[' || b[2] != b']' {
        return None;
    }
    let state = match b[1] {
        b' ' => false,
        b'x' | b'X' => true,
        _ => return None,
    };
    let rest = &s[3..];
    if rest.is_empty() {
        return Some((state, 3));
    }
    if rest.starts_with(' ') || rest.starts_with('\t') {
        let adv = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        Some((state, 3 + adv))
    } else {
        None
    }
}

/// Any construct that terminates a paragraph.
fn starts_new_block(s: &str) -> bool {
    if is_blank(s) {
        return true;
    }
    let (_, q) = strip_quote(s);
    let t = &s[q..];
    heading_marker(t).is_some()
        || fence_marker(t).is_some()
        || is_rule(t)
        || bullet_marker(t).is_some()
        || ordered_marker(t).is_some()
        || t.trim_start().starts_with("$$")
        || html_block_start(t)
}

/// Tag names that open a block of raw HTML on their own line.
///
/// This is CommonMark's "HTML block type 6" list. The distinction matters: a
/// block tag starts a block that runs to the next blank line, so `<div>` is a
/// container, while `<b>` is inline emphasis and must stay inside the
/// paragraph it appears in.
const HTML_BLOCK_TAGS: &[&str] = &[
    "address", "article", "aside", "audio", "base", "basefont", "blockquote", "body", "caption",
    "center", "col", "colgroup", "dd", "details", "dialog", "dir", "div", "dl", "dt", "fieldset",
    "figcaption", "figure", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5",
    "h6", "head", "header", "hr", "html", "iframe", "legend", "li", "link", "main", "menu", "nav",
    "noframes", "ol", "optgroup", "option", "p", "pre", "script", "section", "style", "summary",
    "table", "tbody", "td", "tfoot", "th", "thead", "title", "tr", "track", "ul", "video",
];

/// The tag name at the start of `t`, when `t` begins with `<name`.
///
/// `None` for `<https://…>` — the `:` after the name means this is an autolink,
/// not a tag, which is what keeps `<https://x>` a paragraph.
fn leading_tag_name(t: &str) -> Option<&str> {
    let rest = t.strip_prefix('<')?;
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    match rest.as_bytes().get(end) {
        None | Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') => {
            Some(&rest[..end])
        }
        _ => None,
    }
}

/// True when this line opens a raw-HTML block.
///
/// Two ways in: a known block-level tag, or a line that is nothing but a single
/// tag (`<img … />`), which is how people write self-contained elements with no
/// wrapping container.
fn html_block_start(s: &str) -> bool {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return false;
    }
    let t = &s[ws..];
    // Comments and declarations have nothing to lay out, but they are still
    // HTML blocks — the alternative is a paragraph that renders as nothing but
    // still takes a blank block's worth of height.
    if t.starts_with("<!--") || t.starts_with("<!") || t.starts_with("<?") {
        return true;
    }
    let Some(name) = leading_tag_name(t) else {
        return false;
    };
    if HTML_BLOCK_TAGS.contains(&name.to_ascii_lowercase().as_str()) {
        return true;
    }
    // A self-contained element alone on a line — `<img src="…" />` — is a block
    // by position. Restricted to void elements on purpose: `<b>` on its own
    // line is still emphasis written oddly, not a container.
    if crate::html::is_void(&name.to_ascii_lowercase()) && !t.starts_with("</") {
        if let Some(close) = t.find('>') {
            return t[close + 1..].trim().is_empty();
        }
    }
    false
}

/// `[label]: url "title"`
fn link_definition(s: &str) -> Option<(String, String, String)> {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return None;
    }
    let t = &s[ws..];
    if !t.starts_with('[') {
        return None;
    }
    let close = find_unescaped(t, 1, b']')?;
    let label = &t[1..close];
    if label.is_empty() || label.len() > 999 {
        return None;
    }
    if t.as_bytes().get(close + 1) != Some(&b':') {
        return None;
    }
    let url_part = t[close + 2..].trim();
    if url_part.is_empty() {
        return None;
    }
    let (url, title) = split_link_destination(url_part);
    Some((label.to_lowercase(), url, title))
}

/// `[^name]: body`
fn footnote_definition(s: &str) -> Option<(String, usize)> {
    let (cols, ws) = leading_ws(s);
    if cols > 3 {
        return None;
    }
    let t = &s[ws..];
    if !t.starts_with("[^") {
        return None;
    }
    let close = t.find("]:")?;
    let name = &t[2..close];
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), ws + close + 2))
}

fn find_unescaped(s: &str, from: usize, needle: u8) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == needle => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Split `url "title"` / `<url> (title)` into its two parts.
fn split_link_destination(s: &str) -> (String, String) {
    let s = s.trim();
    if let Some(stripped) = s.strip_prefix('<') {
        if let Some(end) = stripped.find('>') {
            let url = stripped[..end].to_string();
            let title = strip_title(stripped[end + 1..].trim());
            return (url, title);
        }
    }
    // Find a title that starts after the URL.
    if let Some(pos) = find_title_start(s) {
        let url = s[..pos].trim().to_string();
        let title = strip_title(s[pos..].trim());
        return (url, title);
    }
    (s.to_string(), String::new())
}

fn find_title_start(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 0i32;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            b'"' | b'\'' => {
                let end = find_unescaped(s, i + 1, b[i])?;
                if s[end + 1..].trim().is_empty() {
                    return Some(i);
                }
                i += 1;
            }
            _ => {
                let _ = depth;
                i += 1;
            }
        }
    }
    None
}

fn strip_title(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let b = s.as_bytes();
        if (b[0] == b'"' && b[s.len() - 1] == b'"')
            || (b[0] == b'\'' && b[s.len() - 1] == b'\'')
            || (b[0] == b'(' && b[s.len() - 1] == b')')
        {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// GFM table delimiter row, e.g. `| :--- | ---: |`.
fn table_delim(s: &str) -> Option<Vec<Align>> {
    let t = s.trim().trim_matches('|');
    if t.is_empty() {
        return None;
    }
    let mut aligns = Vec::new();
    for cell in t.split('|') {
        let c = cell.trim();
        if c.is_empty() {
            return None;
        }
        let body = c.trim_matches(':');
        if body.is_empty() || !body.bytes().all(|b| b == b'-') {
            return None;
        }
        let left = c.starts_with(':');
        let right = c.ends_with(':');
        aligns.push(match (left, right) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            (false, false) => Align::None,
        });
    }
    if aligns.is_empty() {
        None
    } else {
        Some(aligns)
    }
}

// ===========================================================================
// Blocks
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    pub lang: String,
    pub info: String,
    pub fenced: bool,
    pub fence_ch: u8,
    pub fence_len: usize,
    /// False when the document ends before the closing fence.
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    pub ordered: bool,
    pub number: u64,
    pub delim: char,
    /// Nesting level, 0 for a top-level list.
    pub depth: usize,
    pub task: Option<bool>,
    /// True if this is the last item of its nesting level.
    pub last: bool,
    /// True if the list is "loose" (items separated by blank lines).
    pub loose: bool,
    /// Columns from the line start to where the item's text begins.
    pub content_indent: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BlockKind {
    Blank,
    Paragraph,
    Heading {
        level: u8,
    },
    Code(CodeBlock),
    ListItem(ListItem),
    Table {
        aligns: Vec<Align>,
        rows: Vec<Vec<Range<usize>>>,
        header_rows: usize,
    },
    Rule,
    Math {
        closed: bool,
    },
    /// A raw HTML block. `content` is the source, verbatim, so the renderer
    /// can lay it out and the editor can still show it as text.
    Html,
    LinkDef {
        label: String,
        url: String,
        title: String,
    },
    FootnoteDef {
        name: String,
        /// Content offset where the definition body starts.
        body_off: usize,
    },
}

/// A segment of a block's content that corresponds to a contiguous run of source.
#[derive(Debug, Clone, Copy)]
pub struct Seg {
    /// Offset within `Block::content`.
    pub c: usize,
    /// Offset within the document source.
    pub s: usize,
    /// Length of the source run.
    pub n: usize,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Block {
    pub kind: BlockKind,
    /// Full source range, including the terminating newline.
    pub range: Range<usize>,
    /// Content with structural markers removed.
    pub content: String,
    /// Maps content offsets back to source offsets.
    pub cmap: Vec<Seg>,
    /// Blockquote nesting depth.
    pub quote: usize,
    /// Leading whitespace columns (list nesting / indented code).
    pub indent_cols: usize,
}

#[allow(dead_code)]
impl Block {
    /// The byte range the user actually edits: the block without its trailing
    /// newline, so that Enter at the end of a block always appends a new line
    /// rather than consuming the separator.
    pub fn edit_range(&self, text: &str) -> Range<usize> {
        let end = self.range.end;
        if end > self.range.start && text.as_bytes().get(end - 1) == Some(&b'\n') {
            self.range.start..end - 1
        } else {
            self.range.start..end
        }
    }

    /// Translate an offset in `content` to an offset in the document.
    pub fn src_of(&self, coff: usize) -> usize {
        match self.cmap.iter().rev().find(|seg| seg.c <= coff) {
            Some(seg) => {
                let d = coff - seg.c;
                if d >= seg.n {
                    seg.s + seg.n
                } else {
                    seg.s + d
                }
            }
            None => self.range.start,
        }
    }

    /// Translate a document offset back to an offset in `content`.
    pub fn content_of(&self, soff: usize) -> usize {
        let mut best = 0usize;
        for seg in &self.cmap {
            if soff >= seg.s {
                best = seg.c + (soff - seg.s).min(seg.n);
            }
        }
        best
    }

    pub fn first_line(&self) -> usize {
        self.range.start
    }

    pub fn is_blank(&self) -> bool {
        matches!(self.kind, BlockKind::Blank)
    }

    /// True for blocks that render as block-level media rather than text.
    pub fn is_leaf_visual(&self) -> bool {
        matches!(
            self.kind,
            BlockKind::Code(_)
                | BlockKind::Table { .. }
                | BlockKind::Rule
                | BlockKind::Math { .. }
        )
    }
}

/// Everything the renderer needs about a document, rebuilt on every change.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct Parsed {
    pub blocks: Vec<Block>,
    pub defs: HashMap<String, LinkDef>,
    pub footnotes: Vec<(String, String)>,
    pub footnote_index: HashMap<String, usize>,
}

#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct LinkDef {
    pub url: String,
    pub title: String,
}

struct ContentBuilder {
    s: String,
    map: Vec<Seg>,
}

impl ContentBuilder {
    fn new() -> Self {
        Self {
            s: String::new(),
            map: Vec::new(),
        }
    }

    /// Begin a segment whose source starts at `src_off`.
    fn seg(&mut self, src_off: usize) {
        self.map.push(Seg {
            c: self.s.len(),
            s: src_off,
            n: 0,
        });
    }

    fn push(&mut self, t: &str) {
        self.s.push_str(t);
        if let Some(last) = self.map.last_mut() {
            last.n += t.len();
        }
    }

    fn nl(&mut self) {
        self.s.push('\n');
    }

    fn finish(mut self) -> (String, Vec<Seg>) {
        self.map.retain(|s| s.n > 0 || s.c == 0);
        (self.s, self.map)
    }
}

/// Parse the document into a gapless list of blocks.
pub fn parse(text: &str) -> Parsed {
    let lines = split_lines(text);
    let mut out: Vec<Block> = Vec::new();
    let mut defs: HashMap<String, LinkDef> = HashMap::new();
    let mut footnotes: Vec<(String, String)> = Vec::new();
    let mut list_stack: Vec<usize> = Vec::new();

    let slice = |l: Line| -> &str { &text[l.start..l.end] };

    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        let raw = slice(line);
        let block_start = line.start;
        let (quote, qbytes) = strip_quote(raw);
        let stripped = &raw[qbytes..];

        // ---- blank ------------------------------------------------------
        if is_blank(stripped) {
            let c = ContentBuilder::new();
            out.push(Block {
                kind: BlockKind::Blank,
                range: block_start..line_full_end(text, line),
                content: c.s,
                cmap: c.map,
                quote,
                indent_cols: 0,
            });
            i += 1;
            list_stack.clear();
            continue;
        }

        // ---- ATX heading --------------------------------------------------
        if let Some((level, off)) = heading_marker(stripped) {
            let after = &stripped[off..];
            let lead = after.len() - after.trim_start().len();
            let body_raw = &after[lead..];
            // Trailing `#`s are an optional closing sequence.
            let body = body_raw
                .trim_end()
                .trim_end_matches('#')
                .trim_end();
            let keep = body.len();
            let body = &body_raw[..keep];
            let mut cb = ContentBuilder::new();
            cb.seg(block_start + qbytes + off + lead);
            cb.push(body);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::Heading { level },
                range: block_start..line_full_end(text, line),
                content,
                cmap,
                quote,
                indent_cols: 0,
            });
            i += 1;
            list_stack.clear();
            continue;
        }

        // ---- fenced code -------------------------------------------------
        if let Some(f) = fence_marker(stripped) {
            let mut cb = ContentBuilder::new();
            let info = fence_info(stripped, f);
            let lang = info
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_matches(|c| c == '{' || c == '}' || c == '.')
                .to_string();
            let mut j = i + 1;
            let mut closed = false;
            while j < lines.len() {
                let l2 = lines[j];
                let (q2, qb2) = strip_quote(slice(l2));
                let s2 = &slice(l2)[qb2..];
                if q2 != quote {
                    break;
                }
                if let Some(f2) = fence_marker(s2) {
                    if f2.ch == f.ch && f2.len >= f.len && fence_info(s2, f2).is_empty() {
                        closed = true;
                        break;
                    }
                }
                let body = if s2.len() >= f.indent_bytes {
                    &s2[f.indent_bytes..]
                } else {
                    s2.trim_start()
                };
                cb.seg(l2.start + qb2 + (s2.len() - body.len()));
                cb.push(body);
                cb.nl();
                j += 1;
            }
            // drop the final newline we appended
            if cb.s.ends_with('\n') {
                cb.s.pop();
                if let Some(last) = cb.map.last_mut() {
                    last.n = last.n.saturating_sub(0);
                }
            }
            let end_line = if closed { j } else { j.saturating_sub(1) };
            let end = line_full_end(text, lines[end_line.max(i)]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::Code(CodeBlock {
                    lang,
                    info,
                    fenced: true,
                    fence_ch: f.ch,
                    fence_len: f.len,
                    closed,
                }),
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols: f.indent_cols,
            });
            i = end_line + 1;
            list_stack.clear();
            continue;
        }

        // ---- math block --------------------------------------------------
        if stripped.trim_start().starts_with("$$") {
            let t = stripped.trim_start();
            let inline_close = t.len() > 4 && t[2..].trim_end().ends_with("$$");
            let mut cb = ContentBuilder::new();
            let mut j: usize;
            let mut closed = false;
            if inline_close {
                let inner = &t[2..t.trim_end().len() - 2];
                cb.seg(block_start + (stripped.len() - t.len()) + 2);
                cb.push(inner);
                closed = true;
                j = i + 1;
            } else {
                let after = &t[2..];
                if !after.trim().is_empty() {
                    cb.seg(block_start + (stripped.len() - t.len()) + 2);
                    cb.push(after);
                }
                j = i + 1;
                while j < lines.len() {
                    let l2 = lines[j];
                    let (q2, qb2) = strip_quote(slice(l2));
                    let s2 = &slice(l2)[qb2..];
                    if q2 != quote {
                        break;
                    }
                    if s2.trim_end().ends_with("$$") {
                        let body = s2.trim_end();
                        let body = &body[..body.len() - 2];
                        if !body.trim().is_empty() {
                            cb.seg(l2.start + qb2);
                            cb.push(body);
                        }
                        closed = true;
                        break;
                    }
                    if cb.s.is_empty() {
                        cb.seg(l2.start + qb2);
                        cb.push(s2);
                    } else {
                        cb.seg(l2.start + qb2);
                        cb.push("\n");
                        cb.push(s2);
                    }
                    j += 1;
                }
            }
            let end_line = if closed { j } else { j.saturating_sub(1) };
            let end_line = end_line.min(lines.len() - 1).max(i);
            let end = line_full_end(text, lines[end_line]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::Math { closed },
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols: 0,
            });
            i = end_line + 1;
            list_stack.clear();
            continue;
        }

        // ---- thematic break ----------------------------------------------
        if is_rule(stripped) {
            let c = ContentBuilder::new();
            out.push(Block {
                kind: BlockKind::Rule,
                range: block_start..line_full_end(text, line),
                content: c.s,
                cmap: c.map,
                quote,
                indent_cols: leading_ws(stripped).0,
            });
            i += 1;
            list_stack.clear();
            continue;
        }

        // ---- definitions ---------------------------------------------------
        if let Some((label, url, title)) = link_definition(stripped) {
            defs.entry(label.clone()).or_insert(LinkDef { url, title });
            let c = ContentBuilder::new();
            out.push(Block {
                kind: BlockKind::LinkDef {
                    label,
                    url: String::new(),
                    title: String::new(),
                },
                range: block_start..line_full_end(text, line),
                content: c.s,
                cmap: c.map,
                quote,
                indent_cols: 0,
            });
            i += 1;
            continue;
        }
        if let Some((name, body_off)) = footnote_definition(stripped) {
            let body = stripped[body_off - qbytes..].trim_start();
            footnotes.push((name.clone(), body.to_string()));
            let c = ContentBuilder::new();
            out.push(Block {
                kind: BlockKind::FootnoteDef {
                    name,
                    body_off: block_start + body_off,
                },
                range: block_start..line_full_end(text, line),
                content: c.s,
                cmap: c.map,
                quote,
                indent_cols: 0,
            });
            i += 1;
            continue;
        }

        // ---- list item ------------------------------------------------------
        let bullet = bullet_marker(stripped);
        let ordered = ordered_marker(stripped);
        if bullet.is_some() || ordered.is_some() {
            let (indent_cols, marker_bytes, content_indent, is_ord, number, delim) =
                match (&bullet, &ordered) {
                    (Some(b), _) => (
                        b.cols,
                        b.content_off,
                        b.cols + (b.content_off - b.bytes),
                        false,
                        0u64,
                        '-',
                    ),
                    (_, Some(o)) => (
                        o.cols,
                        o.content_off,
                        o.cols + (o.content_off - o.bytes),
                        true,
                        o.number,
                        o.delim as char,
                    ),
                    _ => unreachable!(),
                };
            // nesting bookkeeping
            while let Some(&top) = list_stack.last() {
                if indent_cols < top {
                    list_stack.pop();
                } else {
                    break;
                }
            }
            if list_stack.last().map_or(true, |&top| indent_cols > top) {
                list_stack.push(indent_cols);
            }
            let depth = list_stack.len().saturating_sub(1);

            let mut cb = ContentBuilder::new();
            let mut task = None;
            let first_body_src = block_start + qbytes + marker_bytes;
            let mut first_body = &stripped[marker_bytes..];
            if let Some((state, adv)) = task_marker(first_body) {
                task = Some(state);
                first_body = &first_body[adv..];
                cb.seg(first_body_src + adv);
            } else {
                cb.seg(first_body_src);
            }
            cb.push(first_body);

            let mut j = i + 1;
            let mut pending_blank = false;
            let mut blank_count = 0usize;
            while j < lines.len() {
                let l2 = lines[j];
                let (q2, qb2) = strip_quote(slice(l2));
                if q2 != quote {
                    break;
                }
                let s2 = &slice(l2)[qb2..];
                if is_blank(s2) {
                    if pending_blank {
                        break;
                    }
                    pending_blank = true;
                    blank_count += 1;
                    j += 1;
                    continue;
                }
                if starts_new_block(s2) || is_rule(s2) {
                    break;
                }
                let (ind, indb) = leading_ws(s2);
                let body = if ind >= content_indent {
                    &s2[indb..]
                } else if pending_blank {
                    break; // lazy continuation cannot follow a blank line
                } else {
                    s2.trim_start()
                };
                if pending_blank {
                    cb.nl();
                    cb.seg(l2.start + qb2 + (s2.len() - body.len()));
                    cb.push(body);
                    pending_blank = false;
                } else {
                    cb.nl();
                    cb.seg(l2.start + qb2 + (s2.len() - body.len()));
                    cb.push(body);
                }
                j += 1;
            }
            let last_line = if pending_blank { j - 1 } else { j - 1 };
            let end = line_full_end(text, lines[last_line.max(i)]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::ListItem(ListItem {
                    ordered: is_ord,
                    number,
                    delim,
                    depth,
                    task,
                    last: false,
                    loose: blank_count > 0,
                    content_indent,
                }),
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols,
            });
            i = j;
            continue;
        }

        // ---- table ----------------------------------------------------------
        if stripped.contains('|') && i + 1 < lines.len() {
            let (q2, qb2) = strip_quote(slice(lines[i + 1]));
            let s2 = &slice(lines[i + 1])[qb2..];
            if q2 == quote {
                if let Some(aligns) = table_delim(s2) {
                    let mut cb = ContentBuilder::new();
                    let mut j = i;
                    let mut first = true;
                    while j < lines.len() {
                        let l2 = lines[j];
                        let (q3, qb3) = strip_quote(slice(l2));
                        if q3 != quote {
                            break;
                        }
                        let s3 = &slice(l2)[qb3..];
                        if is_blank(s3) {
                            break;
                        }
                        // every row after the delimiter must still look like a row
                        if j > i + 1 && !s3.contains('|') {
                            break;
                        }
                        let body = s3.trim();
                        if !first {
                            cb.nl();
                        }
                        cb.seg(l2.start + qb3 + (s3.len() - s3.trim_start().len()));
                        cb.push(body);
                        first = false;
                        j += 1;
                    }
                    let (content, cmap) = cb.finish();
                    let mut rows = recompute_rows(&content);
                    // Row 1 is the `| --- |` alignment row: it carries the column
                    // alignment but is not content, so it must not be rendered.
                    let header_rows = if rows.len() > 1 { 1 } else { 0 };
                    if rows.len() > 1 {
                        rows.remove(1);
                    }
                    let end = line_full_end(text, lines[j.saturating_sub(1).max(i)]);
                    out.push(Block {
                        kind: BlockKind::Table {
                            aligns,
                            rows,
                            header_rows,
                        },
                        range: block_start..end,
                        content,
                        cmap,
                        quote,
                        indent_cols: 0,
                    });
                    i = j;
                    list_stack.clear();
                    continue;
                }
            }
        }

        // ---- indented code ----------------------------------------------------
        if leading_ws(stripped).0 >= 4 {
            let mut cb = ContentBuilder::new();
            let mut j = i;
            let mut last = i;
            let mut first = true;
            while j < lines.len() {
                let l2 = lines[j];
                let (q2, qb2) = strip_quote(slice(l2));
                if q2 != quote {
                    break;
                }
                let s2 = &slice(l2)[qb2..];
                if is_blank(s2) {
                    let more = j + 1 < lines.len() && {
                        let (q3, qb3) = strip_quote(slice(lines[j + 1]));
                        let s3 = &slice(lines[j + 1])[qb3..];
                        q3 == quote && leading_ws(s3).0 >= 4
                    };
                    if !more {
                        break;
                    }
                    cb.nl();
                    last = j;
                    j += 1;
                    continue;
                }
                let (ind, indb) = leading_ws(s2);
                if ind < 4 {
                    break;
                }
                if !first {
                    cb.nl();
                }
                first = false;
                cb.seg(l2.start + qb2 + indb);
                cb.push(&s2[indb..]);
                last = j;
                j += 1;
            }
            let end = line_full_end(text, lines[last]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::Code(CodeBlock {
                    lang: String::new(),
                    info: String::new(),
                    fenced: false,
                    fence_ch: b' ',
                    fence_len: 0,
                    closed: true,
                }),
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols: 4,
            });
            i = j;
            list_stack.clear();
            continue;
        }

        // ---- raw html block ---------------------------------------------------
        //
        // Runs to the next blank line, like CommonMark's type-6 HTML block.
        // Deciding this at block level is what keeps `<div>` a container and
        // `<b>` inline emphasis: only a line that *opens* with a block tag (or
        // is a lone tag) gets here.
        if html_block_start(stripped) {
            let mut cb = ContentBuilder::new();
            let mut j = i;
            let mut last = i;
            while j < lines.len() {
                let l2 = lines[j];
                let (q2, qb2) = strip_quote(slice(l2));
                if q2 != quote {
                    break;
                }
                let s2 = &slice(l2)[qb2..];
                if is_blank(s2) {
                    break;
                }
                if j > i {
                    cb.nl();
                }
                cb.seg(l2.start + qb2);
                cb.push(s2);
                last = j;
                j += 1;
            }
            let end = line_full_end(text, lines[last]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: BlockKind::Html,
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols: 0,
            });
            i = j;
            list_stack.clear();
            continue;
        }

        // ---- paragraph / setext heading ---------------------------------------
        {
            let (ind0, indb0) = leading_ws(stripped);
            let mut cb = ContentBuilder::new();
            let first_body = if ind0 >= 4 {
                &stripped[indb0..]
            } else {
                stripped
            };
            cb.seg(block_start + qbytes + (stripped.len() - first_body.len()));
            cb.push(first_body);
            let mut level: Option<u8> = None;
            let mut j = i + 1;
            let mut last = i;
            while j < lines.len() {
                let l2 = lines[j];
                let (q2, qb2) = strip_quote(slice(l2));
                if q2 != quote {
                    break;
                }
                let s2 = &slice(l2)[qb2..];
                if let Some(lv) = setext_level(s2) {
                    if !is_rule(s2) || lv == 1 {
                        level = Some(lv);
                        last = j;
                        j += 1;
                        break;
                    }
                }
                if is_blank(s2) || starts_new_block(s2) {
                    break;
                }
                let body = s2.trim_start();
                cb.nl();
                cb.seg(l2.start + qb2 + (s2.len() - body.len()));
                cb.push(body);
                last = j;
                j += 1;
            }
            let end = line_full_end(text, lines[last]);
            let (content, cmap) = cb.finish();
            out.push(Block {
                kind: match level {
                    Some(lv) => BlockKind::Heading { level: lv },
                    None => BlockKind::Paragraph,
                },
                range: block_start..end,
                content,
                cmap,
                quote,
                indent_cols: 0,
            });
            i = j;
            list_stack.clear();
            continue;
        }
    }

    // ---- post-pass: list item "last" flags & loose groups ------------------
    let n = out.len();
    for idx in 0..n {
        let (is_item, depth) = match &out[idx].kind {
            BlockKind::ListItem(li) => (true, li.depth),
            _ => (false, 0),
        };
        if !is_item {
            continue;
        }
        let mut last = true;
        for next in out.iter().take(n).skip(idx + 1) {
            match &next.kind {
                BlockKind::Blank => continue,
                BlockKind::ListItem(li) if li.depth >= depth => {
                    last = false;
                    break;
                }
                _ => break,
            }
        }
        let loose = out
            .get(idx + 1)
            .map(|b| matches!(b.kind, BlockKind::Blank))
            .unwrap_or(false);
        if let BlockKind::ListItem(li) = &mut out[idx].kind {
            li.last = last;
            li.loose = loose;
        }
    }

    let footnote_index = footnotes
        .iter()
        .enumerate()
        .map(|(i, (name, _))| (name.clone(), i + 1))
        .collect();

    Parsed {
        blocks: out,
        defs,
        footnotes,
        footnote_index,
    }
}

/// Compute the cell ranges of every row of a table from its content string.
fn recompute_rows(content: &str) -> Vec<Vec<Range<usize>>> {
    let mut rows = Vec::new();
    let mut offset = 0usize;
    for line in content.split('\n') {
        let line_start = offset;
        offset += line.len() + 1;
        let t = line.trim();
        let lead = line.len() - line.trim_start().len();
        let base = line_start + lead;
        let inner = t.trim_matches('|');
        let lead2 = t.len() - t.trim_start_matches('|').len();
        let mut cells = Vec::new();
        let mut cur = 0usize;
        for cell in inner.split('|') {
            let cstart = base + lead2 + cur;
            let cend = cstart + cell.len();
            cells.push(cstart..cend);
            cur += cell.len() + 1;
        }
        rows.push(cells);
    }
    rows
}

// ===========================================================================
// Inline parsing
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub em: bool,
    pub strong: bool,
    pub strike: bool,
    pub code: bool,
    pub highlight: bool,
    /// `<u>` — Markdown has no syntax for this, so it only ever comes from HTML.
    pub underline: bool,
    /// `style="color:#rrggbb"` (or a named colour), packed as 0xRRGGBB.
    ///
    /// Kept as a plain integer rather than a colour type so this module stays
    /// free of the UI toolkit; the renderer unpacks it.
    pub color: Option<u32>,
    /// Literal Markdown punctuation. Only produced when `keep_markers` is set.
    pub marker: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    Span {
        text: String,
        style: Style,
        link: Option<String>,
        src: Range<usize>,
    },
    Image {
        alt: String,
        url: String,
        src: Range<usize>,
    },
    Math {
        tex: String,
        display: bool,
        src: Range<usize>,
    },
    Break {
        hard: bool,
        src: Range<usize>,
    },
    Footnote {
        name: String,
        src: Range<usize>,
    },
}

#[allow(dead_code)]
impl Leaf {
    pub fn src(&self) -> Range<usize> {
        match self {
            Leaf::Span { src, .. }
            | Leaf::Image { src, .. }
            | Leaf::Math { src, .. }
            | Leaf::Break { src, .. }
            | Leaf::Footnote { src, .. } => src.clone(),
        }
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Leaf::Span { .. })
    }

    pub fn plain<'a>(&self, buf: &'a mut String) -> &'a str {
        buf.clear();
        match self {
            Leaf::Span { text, .. } => buf.push_str(text),
            Leaf::Image { alt, .. } => buf.push_str(alt),
            Leaf::Math { tex, .. } => buf.push_str(tex),
            Leaf::Break { .. } => buf.push(' '),
            Leaf::Footnote { name, .. } => buf.push_str(name),
        }
        buf.as_str()
    }
}

pub struct InlineCtx<'a> {
    pub defs: &'a HashMap<String, LinkDef>,
    pub keep_markers: bool,
}

impl Default for InlineCtx<'_> {
    fn default() -> Self {
        static EMPTY: std::sync::OnceLock<HashMap<String, LinkDef>> =
            std::sync::OnceLock::new();
        Self {
            defs: EMPTY.get_or_init(HashMap::new),
            keep_markers: false,
        }
    }
}

/// Parse a block's content into styled leaves. `src` ranges are relative to
/// `src`, so callers map them through [`Block::src_of`] when they need source
/// offsets.
pub fn parse_inline(src: &str, ctx: &InlineCtx) -> Vec<Leaf> {
    let mut out = Vec::new();
    let p = InlineParser { s: src, ctx };
    p.parse_range(0, src.len(), Style::default(), None, &mut out);
    out
}

struct InlineParser<'a> {
    s: &'a str,
    ctx: &'a InlineCtx<'a>,
}

/// The style an inline HTML element contributes, or `None` when the tag is not
/// one we recognise.
///
/// Recognising a tag means "this is markup, not text": the tag itself is
/// consumed and only its effect survives. Anything not in this list — `<3`,
/// `<https://x>`, and genuinely unknown elements — is left alone for the code
/// span path, so nothing is silently deleted.
fn inline_tag_style(name: &str, attrs: &str, style: Style) -> Option<Style> {
    let s = match name {
        "b" | "strong" => Style {
            strong: true,
            ..style
        },
        "i" | "em" | "cite" | "var" | "dfn" | "address" => Style { em: true, ..style },
        "u" | "ins" => Style {
            underline: true,
            ..style
        },
        "s" | "del" | "strike" => Style {
            strike: true,
            ..style
        },
        "code" | "kbd" | "samp" | "tt" => Style { code: true, ..style },
        "mark" => Style {
            highlight: true,
            ..style
        },
        "a" | "span" | "font" => {
            let css = crate::html::attr_value(attrs, "style").unwrap_or_default();
            let from_css = crate::html::css_prop(&css, "color")
                .and_then(|v| crate::html::parse_color_packed(&v));
            let from_attr = crate::html::attr_value(attrs, "color")
                .and_then(|v| crate::html::parse_color_packed(&v));
            match from_css.or(from_attr) {
                Some(c) => Style {
                    color: Some(c),
                    ..style
                },
                None => style,
            }
        }
        // Elements that only group or structure: nothing to add, but they are
        // still markup, so the tags themselves disappear.
        "abbr" | "bdi" | "bdo" | "big" | "data" | "label" | "nobr" | "output" | "q"
        | "rp" | "rt" | "ruby" | "small" | "sub" | "sup" | "time" | "div" | "p"
        | "section" | "article" | "aside" | "blockquote" | "body" | "center" | "dd"
        | "dl" | "dt" | "fieldset" | "figcaption" | "figure" | "footer" | "form"
        | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "html" | "li"
        | "main" | "nav" | "noscript" | "ol" | "option" | "select" | "table"
        | "tbody" | "td" | "template" | "tfoot" | "th" | "thead" | "tr" | "ul"
        | "audio" | "canvas" | "caption" | "colgroup" | "iframe" | "object"
        | "picture" | "video" | "summary" | "details" | "pre" => style,
        _ => return None,
    };
    Some(s)
}

fn is_ws(c: Option<char>) -> bool {
    match c {
        None => true,
        Some(c) => c.is_whitespace(),
    }
}

fn is_punct(c: Option<char>) -> bool {
    match c {
        None => false,
        Some(c) => {
            c.is_ascii_punctuation()
                || matches!(
                    c,
                    '。' | '，' | '、' | '；' | '：' | '？' | '！' | '（' | '）' | '【' | '】'
                        | '《' | '》' | '「' | '」' | '『' | '』' | '…' | '—' | '·'
                )
        }
    }
}

fn ascii_punct(c: u8) -> bool {
    matches!(c, b'!'..=b'/' | b':'..=b'@' | b'['..=b'`' | b'{'..=b'~')
}

impl<'a> InlineParser<'a> {
    fn bytes(&self) -> &'a [u8] {
        self.s.as_bytes()
    }

    fn byte(&self, i: usize) -> Option<u8> {
        self.bytes().get(i).copied()
    }

    fn char_at(&self, i: usize) -> char {
        self.s[i..].chars().next().unwrap_or('\0')
    }

    fn char_before(&self, i: usize) -> Option<char> {
        if i == 0 {
            None
        } else {
            self.s[..i].chars().next_back()
        }
    }

    fn char_after(&self, i: usize) -> Option<char> {
        if i >= self.s.len() {
            None
        } else {
            self.s[i..].chars().next()
        }
    }

    fn push_text(
        out: &mut Vec<Leaf>,
        text: &str,
        style: Style,
        link: Option<&str>,
        src: Range<usize>,
    ) {
        if text.is_empty() {
            return;
        }
        if let Some(Leaf::Span {
            text: last_text,
            style: last_style,
            link: last_link,
            src: last_src,
        }) = out.last_mut()
        {
            if *last_style == style
                && last_link.as_deref() == link
                && last_src.end == src.start
            {
                last_text.push_str(text);
                last_src.end = src.end;
                return;
            }
        }
        out.push(Leaf::Span {
            text: text.to_string(),
            style,
            link: link.map(|s| s.to_string()),
            src,
        });
    }

    fn marker(out: &mut Vec<Leaf>, text: &str, src: Range<usize>) {
        if text.is_empty() {
            return;
        }
        out.push(Leaf::Span {
            text: text.to_string(),
            style: Style {
                marker: true,
                ..Default::default()
            },
            link: None,
            src,
        });
    }

    /// `<!-- … -->` renders as nothing.
    ///
    /// Returns the offset past the comment. In the source view the comment is
    /// kept as a marker, because the caret has to be able to walk over the text
    /// it is standing on.
    fn try_html_comment(&self, at: usize, to: usize, out: &mut Vec<Leaf>) -> Option<usize> {
        let rest = &self.s[at..to];
        if !rest.starts_with("<!--") {
            return None;
        }
        let end = rest.find("-->").map(|p| at + p + 3).unwrap_or(to);
        if self.ctx.keep_markers {
            Self::marker(out, &self.s[at..end], at..end);
        }
        Some(end)
    }

    /// The `</name>` matching an open `name` at or after `from`.
    ///
    /// Returns `(start of the close tag, offset past it)`, counting nested
    /// same-name elements so `<span><span>x</span></span>` closes correctly.
    fn find_close(&self, name: &str, from: usize, to: usize) -> Option<(usize, usize)> {
        let mut depth = 0usize;
        let mut p = from;
        while p < to {
            let lt = self.s[p..to].find('<')? + p;
            let gt = self.s[lt..to].find('>')? + lt;
            let head = &self.s[lt + 1..gt];
            let closing = head.starts_with('/');
            let body = head.trim_start_matches('/');
            let nlen = body
                .find(|c: char| c.is_whitespace() || c == '/')
                .unwrap_or(body.len());
            if body[..nlen].eq_ignore_ascii_case(name) {
                if closing {
                    if depth == 0 {
                        return Some((lt, gt + 1));
                    }
                    depth -= 1;
                } else if !crate::html::is_void(&body[..nlen].to_ascii_lowercase()) {
                    depth += 1;
                }
            }
            p = gt + 1;
        }
        None
    }

    /// An inline HTML tag at `at`, with its closing `>` at `gt`.
    ///
    /// Returns the offset just past what the tag consumed, or `None` when this
    /// is not a tag we understand — in which case the caller shows it as a code
    /// span, which is what a Markdown reader expects to see for markup it
    /// cannot act on.
    ///
    /// A container tag consumes its *whole element*: the style it applies ends
    /// where it ends, and the alternative is a style stack threaded through
    /// every recursive call for a feature (`<span style="color:…">` around half
    /// a paragraph) that is rare enough not to be worth it.
    fn try_html_tag(
        &self,
        at: usize,
        gt: usize,
        to: usize,
        style: Style,
        link: Option<&str>,
        out: &mut Vec<Leaf>,
    ) -> Option<usize> {
        let head = &self.s[at + 1..gt];
        let closing = head.starts_with('/');
        let body = head.trim_start_matches('/');
        let nlen = body
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(body.len());
        let name = body[..nlen].to_ascii_lowercase();
        if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
            return None;
        }
        let attrs = &body[nlen..];

        // Void elements: they produce something, they never wrap anything.
        match name.as_str() {
            "br" => {
                out.push(Leaf::Break {
                    hard: true,
                    src: at..gt + 1,
                });
                return Some(gt + 1);
            }
            "hr" => {
                out.push(Leaf::Break {
                    hard: true,
                    src: at..gt + 1,
                });
                if !self.ctx.keep_markers {
                    // A rule in the flow is just a break; the block case draws
                    // the line.
                }
                return Some(gt + 1);
            }
            "img" => {
                let url = crate::html::attr_value(attrs, "src").unwrap_or_default();
                let alt = crate::html::attr_value(attrs, "alt").unwrap_or_default();
                if !url.is_empty() {
                    out.push(Leaf::Image {
                        alt,
                        url,
                        src: at..gt + 1,
                    });
                }
                return Some(gt + 1);
            }
            "wbr" | "area" | "input" | "meta" | "link" | "source" | "track" | "col" => {
                return Some(gt + 1)
            }
            _ => {}
        }

        if closing {
            // A close tag with nothing open: the element was opened in another
            // block, or the markup is broken. Either way it is not text.
            if self.ctx.keep_markers {
                Self::marker(out, &self.s[at..gt + 1], at..gt + 1);
            }
            return Some(gt + 1);
        }

        let new_style = inline_tag_style(&name, attrs, style)?;
        let href = if name == "a" {
            crate::html::attr_value(attrs, "href")
        } else {
            None
        };
        let (close_at, after) = self.find_close(&name, gt + 1, to).unwrap_or((to, to));

        if self.ctx.keep_markers {
            Self::marker(out, &self.s[at..gt + 1], at..gt + 1);
        }
        let owned;
        let inner_link = match &href {
            Some(h) => {
                owned = h.clone();
                Some(owned.as_str())
            }
            None => link,
        };
        self.parse_range(gt + 1, close_at, new_style, inner_link, out);
        if self.ctx.keep_markers && after > close_at {
            Self::marker(out, &self.s[close_at..after], close_at..after);
        }
        Some(after)
    }

    fn parse_range(
        &self,
        from: usize,
        to: usize,
        style: Style,
        link: Option<&str>,
        out: &mut Vec<Leaf>,
    ) {
        let mut i = from;
        while i < to {
            let c = self.char_at(i);
            let clen = c.len_utf8();

            // ---- escapes -------------------------------------------------
            if c == '\\' {
                if let Some(n) = self.byte(i + 1) {
                    if n == b'\n' {
                        out.push(Leaf::Break {
                            hard: true,
                            src: i..i + 2,
                        });
                        i += 2;
                        continue;
                    }
                    if ascii_punct(n) {
                        let ch = n as char;
                        Self::push_text(out, &ch.to_string(), style, link, i..i + 2);
                        i += 2;
                        continue;
                    }
                }
                Self::push_text(out, "\\", style, link, i..i + 1);
                i += 1;
                continue;
            }

            // ---- line break ----------------------------------------------
            if c == '\n' {
                let before = &self.s[from..i];
                let hard = before.ends_with("  ") || before.ends_with('\\');
                let src = if hard && before.ends_with("  ") {
                    i.saturating_sub(2)..i + 1
                } else {
                    i..i + 1
                };
                out.push(Leaf::Break { hard, src });
                i += 1;
                continue;
            }

            // ---- code span -----------------------------------------------
            if c == '`' {
                if let Some(end) = self.try_code(i, to) {
                    let run = self.bytes()[i..].iter().take_while(|&&b| b == b'`').count();
                    let (mut inner, inner_src) = (self.s[i + run..end].to_string(), i + run..end);
                    if inner.starts_with(' ')
                        && inner.ends_with(' ')
                        && !inner.trim().is_empty()
                    {
                        inner = inner[1..inner.len() - 1].to_string();
                    }
                    if self.ctx.keep_markers {
                        Self::marker(out, &self.s[i..i + run], i..i + run);
                        Self::push_text(
                            out,
                            &inner,
                            Style {
                                code: true,
                                ..style
                            },
                            link,
                            inner_src.clone(),
                        );
                        Self::marker(out, &self.s[end..end + run], end..end + run);
                    } else {
                        Self::push_text(
                            out,
                            &inner,
                            Style {
                                code: true,
                                ..style
                            },
                            link,
                            inner_src,
                        );
                    }
                    i = end + run;
                    continue;
                }
            }

            // ---- math -------------------------------------------------------
            if c == '$' {
                if let Some((end, display)) = self.try_math(i, to) {
                    let open = if display { 2 } else { 1 };
                    let tex = self.s[i + open..end].trim().to_string();
                    out.push(Leaf::Math {
                        tex,
                        display,
                        src: i..end + open,
                    });
                    i = end + open;
                    continue;
                }
            }

            // ---- image --------------------------------------------------------
            if c == '!' && self.byte(i + 1) == Some(b'[') {
                if let Some((consumed_end, alt, url)) = self.try_link_like(i + 1, to) {
                    out.push(Leaf::Image {
                        alt,
                        url,
                        src: i..consumed_end,
                    });
                    i = consumed_end;
                    continue;
                }
            }

            // ---- link / footnote ------------------------------------------------
            if c == '[' {
                if self.byte(i + 1) == Some(b'^') {
                    if let Some(close) = self.s[i..to].find(']') {
                        let name = &self.s[i + 2..i + close];
                        if !name.is_empty() && !name.contains(' ') {
                            out.push(Leaf::Footnote {
                                name: name.to_string(),
                                src: i..i + close + 1,
                            });
                            i = i + close + 1;
                            continue;
                        }
                    }
                }
                if let Some((consumed_end, text, url)) = self.try_link_like(i, to) {
                    let inner_start = i + 1;
                    let inner_end = self.s[i..].find(']').map(|p| i + p).unwrap_or(inner_start);
                    if self.ctx.keep_markers {
                        Self::marker(out, "[", i..i + 1);
                        self.parse_range(inner_start, inner_end, style, Some(&url), out);
                        let close = &self.s[inner_end..consumed_end];
                        Self::marker(out, close, inner_end..consumed_end);
                    } else {
                        self.parse_range(inner_start, inner_end, style, Some(&url), out);
                    }
                    let _ = text;
                    i = consumed_end;
                    continue;
                }
            }

            // ---- autolink / raw html --------------------------------------------
            if c == '<' {
                if let Some(end) = self.s[i..to].find('>').map(|p| i + p) {
                    let inner = &self.s[i + 1..end];
                    let is_url = inner.starts_with("http://")
                        || inner.starts_with("https://")
                        || inner.starts_with("mailto:")
                        || (inner.contains('@') && !inner.contains(' '));
                    if is_url {
                        let url = if inner.contains('@') && !inner.starts_with("mailto:") {
                            format!("mailto:{inner}")
                        } else {
                            inner.to_string()
                        };
                        if self.ctx.keep_markers {
                            Self::marker(out, "<", i..i + 1);
                        }
                        Self::push_text(out, inner, style, Some(&url), i + 1..end);
                        if self.ctx.keep_markers {
                            Self::marker(out, ">", end..end + 1);
                        }
                        i = end + 1;
                        continue;
                    }
                    // An HTML comment renders as nothing at all. In the source
                    // view it is a marker, so the text the caret walks through
                    // still adds up to the block.
                    if let Some(after) = self.try_html_comment(i, to, out) {
                        i = after;
                        continue;
                    }
                    if let Some(after) = self.try_html_tag(i, end, to, style, link, out) {
                        i = after;
                        continue;
                    }
                    let looks_like_tag = inner
                        .chars()
                        .next()
                        .map(|ch| ch.is_ascii_alphabetic() || ch == '/' || ch == '!')
                        .unwrap_or(false)
                        && !inner.contains("  ");
                    if looks_like_tag {
                        Self::push_text(
                            out,
                            &self.s[i..end + 1],
                            Style {
                                code: true,
                                ..style
                            },
                            link,
                            i..end + 1,
                        );
                        i = end + 1;
                        continue;
                    }
                }
            }

            // ---- entity ------------------------------------------------------------
            if c == '&' {
                if let Some((decoded, end)) = decode_entity(self.s, i, to) {
                    Self::push_text(out, &decoded, style, link, i..end);
                    i = end;
                    continue;
                }
            }

            // ---- emphasis ------------------------------------------------------------
            if c == '*' || c == '_' {
                if let Some(end) = self.try_emphasis(i, to, style, link, out) {
                    i = end;
                    continue;
                }
            }

            if c == '~' && self.byte(i + 1) == Some(b'~') {
                if let Some(end) = self.try_delimited(
                    i,
                    to,
                    b'~',
                    2,
                    Style {
                        strike: true,
                        ..style
                    },
                    style,
                    link,
                    out,
                ) {
                    i = end;
                    continue;
                }
            }

            if c == '=' && self.byte(i + 1) == Some(b'=') {
                if let Some(end) = self.try_delimited(
                    i,
                    to,
                    b'=',
                    2,
                    Style {
                        highlight: true,
                        ..style
                    },
                    style,
                    link,
                    out,
                ) {
                    i = end;
                    continue;
                }
            }

            // ---- ordinary text ----------------------------------------------------------
            let s = &self.s[i..(i + clen).min(to)];
            Self::push_text(out, s, style, link, i..i + clen);
            i += clen;
        }
    }

    // ---------------------------------------------------------------- helpers

    /// Code span starting at `i`; returns the offset of the closing run.
    fn try_code(&self, i: usize, to: usize) -> Option<usize> {
        let b = self.bytes();
        let run = b[i..to].iter().take_while(|&&c| c == b'`').count();
        let mut j = i + run;
        while j < to {
            if b[j] == b'`' {
                let n = b[j..to].iter().take_while(|&&c| c == b'`').count();
                if n == run {
                    return Some(j);
                }
                j += n;
            } else {
                j += 1;
            }
        }
        None
    }

    /// Inline math starting at `$`; returns (offset of the closing `$`, display).
    fn try_math(&self, i: usize, to: usize) -> Option<(usize, bool)> {
        let b = self.bytes();
        let display = b.get(i + 1) == Some(&b'$');
        let open = if display { 2 } else { 1 };
        let after = self.char_after(i + open)?;
        if after.is_whitespace() {
            return None;
        }
        if !display && after == '$' {
            return None;
        }
        let mut j = i + open;
        while j < to {
            match b[j] {
                b'\\' => {
                    j += 2;
                }
                b'\n' => return None,
                b'$' => {
                    let n = b[j..to].iter().take_while(|&&c| c == b'$').count();
                    if n >= open {
                        let before = self.char_before(j);
                        if before.map(|c| c.is_whitespace()).unwrap_or(true) {
                            return None;
                        }
                        let following = self.char_after(j + n);
                        if !display && following.map(|c| c.is_ascii_digit()).unwrap_or(false) {
                            return None;
                        }
                        return Some((j, display));
                    }
                    j += n;
                }
                _ => j += 1,
            }
        }
        None
    }

    /// `[label](dest)` / `[label][ref]` / `[label]`.
    /// `open` must point at the `[`. Returns (end, label text, url).
    fn try_link_like(&self, open: usize, to: usize) -> Option<(usize, String, String)> {
        let b = self.bytes();
        let mut depth = 0i32;
        let mut j = open;
        let mut close = None;
        while j < to {
            match b[j] {
                b'\\' => j += 2,
                b'[' => {
                    depth += 1;
                    j += 1;
                }
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(j);
                        break;
                    }
                    j += 1;
                }
                _ => j += 1,
            }
        }
        let close = close?;
        let label = self.s[open + 1..close].to_string();
        let after = close + 1;
        match b.get(after) {
            Some(b'(') => {
                let mut d = 1i32;
                let mut k = after + 1;
                while k < to {
                    match b[k] {
                        b'\\' => k += 2,
                        b'(' => {
                            d += 1;
                            k += 1;
                        }
                        b')' => {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                            k += 1;
                        }
                        _ => k += 1,
                    }
                }
                if d != 0 || k >= to {
                    return None;
                }
                let dest = &self.s[after + 1..k];
                let (url, _title) = split_link_destination(dest);
                Some((k + 1, label, url))
            }
            Some(b'[') => {
                let ref_close = self.s[after..to].find(']').map(|p| after + p)?;
                let key = self.s[after + 1..ref_close].to_string();
                let key = if key.is_empty() { label.clone() } else { key };
                let def = self.ctx.defs.get(&key.to_lowercase())?;
                Some((ref_close + 1, label, def.url.clone()))
            }
            _ => {
                let def = self.ctx.defs.get(&label.to_lowercase())?;
                Some((close + 1, label, def.url.clone()))
            }
        }
    }

    fn can_open(&self, ch: u8, i: usize, run: usize) -> bool {
        let before = self.char_before(i);
        let after = self.char_after(i + run);
        let after_ws = is_ws(after);
        let before_ws = is_ws(before);
        let after_p = is_punct(after);
        let before_p = is_punct(before);
        let left = !after_ws && (!after_p || before_ws || before_p);
        let right = !before_ws && (!before_p || after_ws || after_p);
        match ch {
            b'_' => left && (!right || before_p),
            _ => left,
        }
    }

    fn can_close(&self, ch: u8, i: usize, run: usize) -> bool {
        let before = self.char_before(i);
        let after = self.char_after(i + run);
        let after_ws = is_ws(after);
        let before_ws = is_ws(before);
        let after_p = is_punct(after);
        let before_p = is_punct(before);
        let left = !after_ws && (!after_p || before_ws || before_p);
        let right = !before_ws && (!before_p || after_ws || after_p);
        match ch {
            b'_' => right && (!left || after_p),
            _ => right,
        }
    }

    fn try_emphasis(
        &self,
        i: usize,
        to: usize,
        style: Style,
        link: Option<&str>,
        out: &mut Vec<Leaf>,
    ) -> Option<usize> {
        let ch = self.bytes()[i];
        let run = self.bytes()[i..to].iter().take_while(|&&c| c == ch).count();
        if run == 0 || !self.can_open(ch, i, run) {
            return None;
        }
        // find a closing run
        let mut j = i + run;
        let b = self.bytes();
        while j < to {
            match b[j] {
                b'\\' => {
                    j += 2;
                    continue;
                }
                c if c == ch => {
                    let n = b[j..to].iter().take_while(|&&c| c == ch).count();
                    if self.can_close(ch, j, n) {
                        let use_len = if run >= 3 && n >= 3 {
                            3
                        } else if run >= 2 && n >= 2 {
                            2
                        } else {
                            1
                        };
                        let inner_style = if use_len == 3 {
                            Style {
                                em: true,
                                strong: true,
                                ..style
                            }
                        } else if use_len == 2 {
                            Style {
                                strong: true,
                                ..style
                            }
                        } else {
                            Style {
                                em: true,
                                ..style
                            }
                        };
                        if self.ctx.keep_markers {
                            Self::marker(out, &self.s[i..i + use_len], i..i + use_len);
                        }
                        self.parse_range(i + use_len, j, inner_style, link, out);
                        if self.ctx.keep_markers {
                            Self::marker(out, &self.s[j..j + use_len], j..j + use_len);
                        }
                        return Some(j + use_len);
                    }
                    j += n;
                }
                _ => j += 1,
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn try_delimited(
        &self,
        i: usize,
        to: usize,
        ch: u8,
        want: usize,
        inner_style: Style,
        _outer: Style,
        link: Option<&str>,
        out: &mut Vec<Leaf>,
    ) -> Option<usize> {
        let b = self.bytes();
        let run = b[i..to].iter().take_while(|&&c| c == ch).count();
        if run < want || !self.can_open(ch, i, run) {
            return None;
        }
        let mut j = i + want;
        while j < to {
            if b[j] == ch {
                let n = b[j..to].iter().take_while(|&&c| c == ch).count();
                if n >= want && self.can_close(ch, j, n) {
                    if self.ctx.keep_markers {
                        Self::marker(out, &self.s[i..i + want], i..i + want);
                    }
                    self.parse_range(i + want, j, inner_style, link, out);
                    if self.ctx.keep_markers {
                        Self::marker(out, &self.s[j..j + want], j..j + want);
                    }
                    return Some(j + want);
                }
                j += n;
            } else {
                j += 1;
            }
        }
        None
    }
}

fn decode_entity(s: &str, i: usize, to: usize) -> Option<(String, usize)> {
    let rest = &s[i..to];
    let semi = rest.find(';')?;
    if semi > 32 {
        return None;
    }
    let body = &rest[1..semi];
    let decoded = match body {
        "amp" => "&".to_string(),
        "lt" => "<".to_string(),
        "gt" => ">".to_string(),
        "quot" => "\"".to_string(),
        "apos" => "'".to_string(),
        "nbsp" => "\u{00A0}".to_string(),
        "hellip" => "…".to_string(),
        "mdash" => "—".to_string(),
        "ndash" => "–".to_string(),
        "times" => "×".to_string(),
        "divide" => "÷".to_string(),
        "copy" => "©".to_string(),
        "reg" => "®".to_string(),
        "trade" => "™".to_string(),
        "larr" => "←".to_string(),
        "rarr" => "→".to_string(),
        _ => {
            if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                let n = u32::from_str_radix(hex, 16).ok()?;
                char::from_u32(n)?.to_string()
            } else if let Some(dec) = body.strip_prefix('#') {
                let n: u32 = dec.parse().ok()?;
                char::from_u32(n)?.to_string()
            } else {
                return None;
            }
        }
    };
    Some((decoded, i + semi + 1))
}

/// Length, in bytes, of the structural marker prefix of a source line, so the
/// editor can dim `#`, `>`, `- ` etc. while the block is being edited.
pub fn line_marker_len(kind: &BlockKind, line_in_block: usize, line: &str) -> usize {
    let (_, qbytes) = strip_quote(line);
    if line_in_block > 0 {
        // continuation lines only repeat the blockquote prefix
        return match kind {
            BlockKind::Code(_) | BlockKind::Math { .. } => 0,
            BlockKind::ListItem(_) => qbytes,
            _ => qbytes,
        };
    }
    let stripped = &line[qbytes..];
    let extra = match kind {
        BlockKind::Heading { .. } => heading_marker(stripped).map(|(_, n)| n).unwrap_or(0),
        BlockKind::ListItem(li) => {
            let marker = if li.ordered {
                ordered_marker(stripped).map(|o| o.content_off).unwrap_or(0)
            } else {
                bullet_marker(stripped)
                    .map(|b| b.content_off)
                    .unwrap_or(0)
            };
            let mut n = marker;
            if li.task.is_some() {
                if let Some((_, adv)) = task_marker(&stripped[marker.min(stripped.len())..]) {
                    n += adv;
                }
            }
            n
        }
        BlockKind::Code(cb) if cb.fenced => {
            let info = fence_info(stripped, fence_marker(stripped).unwrap_or(Fence {
                ch: cb.fence_ch,
                len: cb.fence_len,
                indent_cols: 0,
                indent_bytes: 0,
            }));
            stripped.len() - info.len()
        }
        BlockKind::Math { .. } => stripped.trim_start().len().min(2).max(
            stripped.trim_start().starts_with("$$").then_some(2).unwrap_or(0),
        ),
        _ => 0,
    };
    qbytes + extra
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<String> {
        parse(text)
            .blocks
            .into_iter()
            .map(|b| format!("{:?}", b.kind))
            .collect()
    }

    #[test]
    fn blocks_tile_the_document() {
        for src in [
            "",
            "a",
            "a\n",
            "# h\n\npara\nline\n\n- a\n- b\n",
            "> quote\n> more\n\n```rust\nfn x() {}\n```\n",
            "| a | b |\n| --- | ---: |\n| 1 | 2 |\n",
            "- [x] done\n  - nested\n\n1. one\n2. two\n",
            "para\n===\n\n$$x^2$$\n",
            "text\n\n    code\n",
            "a\n\n\n\nb\n",
            "[ref]: http://x\n\nsee [ref]\n",
        ] {
            let p = parse(src);
            let rebuilt: String = p.blocks.iter().map(|b| &src[b.range.clone()]).collect();
            assert_eq!(rebuilt, src, "blocks do not tile: {src:?}");
            // no empty blocks
            for b in &p.blocks {
                assert!(b.range.end >= b.range.start);
            }
        }
    }

    #[test]
    fn basic_blocks() {
        let k = kinds("# h\n\npara\n\n- a\n- b\n");
        assert_eq!(
            k,
            vec![
                "Heading { level: 1 }".to_string(),
                "Blank".into(),
                "Paragraph".into(),
                "Blank".into(),
                "ListItem(ListItem { ordered: false, number: 0, delim: '-', depth: 0, task: None, last: false, loose: false, content_indent: 2 })".into(),
                "ListItem(ListItem { ordered: false, number: 0, delim: '-', depth: 0, task: None, last: true, loose: false, content_indent: 2 })".into(),
            ]
        );
    }

    #[test]
    fn nesting_depth() {
        let p = parse("- a\n  - b\n    - c\n- d\n");
        let depths: Vec<usize> = p
            .blocks
            .iter()
            .filter_map(|b| match &b.kind {
                BlockKind::ListItem(li) => Some(li.depth),
                _ => None,
            })
            .collect();
        assert_eq!(depths, vec![0, 1, 2, 0]);
    }

    #[test]
    fn emphasis() {
        let ctx = InlineCtx::default();
        let leaves = parse_inline("**bold** and *it* and `c` and ~~s~~", &ctx);
        let styles: Vec<(String, Style)> = leaves
            .iter()
            .filter_map(|l| match l {
                Leaf::Span { text, style, .. } => Some((text.clone(), *style)),
                _ => None,
            })
            .collect();
        assert!(styles.iter().any(|(t, s)| t == "bold" && s.strong));
        assert!(styles.iter().any(|(t, s)| t == "it" && s.em));
        assert!(styles.iter().any(|(t, s)| t == "c" && s.code));
        assert!(styles.iter().any(|(t, s)| t == "s" && s.strike));
    }

    #[test]
    fn intraword_underscore_stays_literal() {
        let ctx = InlineCtx::default();
        let leaves = parse_inline("snake_case_name", &ctx);
        assert_eq!(leaves.len(), 1);
        assert_eq!(leaves[0].plain(&mut String::new()), "snake_case_name");
    }

    #[test]
    fn links_and_images() {
        let ctx = InlineCtx::default();
        let leaves = parse_inline("see [a](http://x) ![alt](img.png)", &ctx);
        assert!(leaves
            .iter()
            .any(|l| matches!(l, Leaf::Span { link: Some(u), .. } if u == "http://x")));
        assert!(leaves
            .iter()
            .any(|l| matches!(l, Leaf::Image { url, .. } if url == "img.png")));
    }

    #[test]
    fn marker_mode_preserves_source_length() {
        let defs = HashMap::new();
        let ctx = InlineCtx {
            defs: &defs,
            keep_markers: true,
        };
        let src = "**bold**";
        let leaves = parse_inline(src, &ctx);
        let total: usize = leaves.iter().map(|l| l.src().len()).sum();
        assert_eq!(total, src.len());
        assert_eq!(leaves[0].plain(&mut String::new()), "**");
    }

    #[test]
    fn table_rows() {
        let p = parse("| a | b |\n| --- | ---: |\n| 1 | 2 |\n");
        let (t, header_rows) = p
            .blocks
            .iter()
            .find_map(|b| match &b.kind {
                BlockKind::Table {
                    rows, header_rows, ..
                } => Some((rows.clone(), *header_rows)),
                _ => None,
            })
            .expect("table");
        // The `| --- |` alignment row is metadata, not content.
        assert_eq!(header_rows, 1);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].len(), 2);
        let content = &p
            .blocks
            .iter()
            .find(|b| matches!(b.kind, BlockKind::Table { .. }))
            .unwrap()
            .content;
        assert_eq!(content[t[0][0].clone()].trim(), "a");
        assert_eq!(content[t[1][0].clone()].trim(), "1");
        assert_eq!(content[t[1][1].clone()].trim(), "2");
    }

    // ---- raw HTML ------------------------------------------------------

    #[test]
    fn a_block_tag_opens_an_html_block() {
        for line in [
            "<div style=\"x\">",
            "  <table>",
            "<details>",
            "<!-- a comment -->",
            "<img src=\"a.png\" />",
            "</div>",
        ] {
            assert!(html_block_start(line), "{line:?} should open a block");
        }
    }

    #[test]
    fn an_autolink_is_not_an_html_block() {
        // The `:` after the name is what separates a URL from a tag. Getting
        // this wrong turns every autolink paragraph into raw markup.
        assert!(!html_block_start("<https://example.com>"));
        assert!(!html_block_start("<mailto:h@example.com>"));
        assert!(!html_block_start("<b>bold</b> and more text"));
        assert!(!html_block_start("plain text"));
        // Inline elements are emphasis, not structure.
        assert!(!html_block_start("<b>"));
        assert!(!html_block_start("<span style=\"color:red\">"));
    }

    #[test]
    fn a_html_block_runs_to_the_next_blank_line() {
        let src = "<div>\n  <strong>x</strong>\n</div>\n\ntail paragraph\n";
        let p = parse(src);
        assert!(
            matches!(p.blocks[0].kind, BlockKind::Html),
            "{:?}",
            p.blocks[0].kind
        );
        assert_eq!(p.blocks[0].content, "<div>\n  <strong>x</strong>\n</div>");
        assert!(
            matches!(p.blocks[2].kind, BlockKind::Paragraph),
            "the paragraph after the blank line must not be swallowed: {:?}",
            p.blocks[2].kind
        );
    }

    #[test]
    fn a_html_block_is_laid_out_where_it_starts() {
        // The block runs to the *blank line*, so a heading inside it does not
        // become a heading.
        let p = parse("<div>\n# not a heading\n</div>\n");
        assert!(matches!(p.blocks[0].kind, BlockKind::Html));
        assert_eq!(p.blocks.len(), 1);
    }

    fn inline(src: &str) -> Vec<Leaf> {
        let defs = HashMap::new();
        parse_inline(
            src,
            &InlineCtx {
                defs: &defs,
                keep_markers: false,
            },
        )
    }

    fn style_of(leaves: &[Leaf], text: &str) -> Style {
        leaves
            .iter()
            .find_map(|l| match l {
                Leaf::Span { text: t, style, .. } if t == text => Some(*style),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{text:?} not found in {leaves:?}"))
    }

    #[test]
    fn inline_html_tags_style_their_content() {
        let l = inline("<b>bold</b> <i>it</i> <u>under</u> <s>gone</s> <code>c</code>");
        assert!(style_of(&l, "bold").strong);
        assert!(style_of(&l, "it").em);
        assert!(style_of(&l, "under").underline);
        assert!(style_of(&l, "gone").strike);
        assert!(style_of(&l, "c").code);
        // The tags themselves are markup, not text.
        let joined: String = l
            .iter()
            .map(|x| x.plain(&mut String::new()).to_string())
            .collect();
        assert!(!joined.contains('<'), "tags leaked into the text: {joined:?}");
    }

    #[test]
    fn a_colour_from_an_inline_style_survives() {
        let l = inline(r#"<span style="color:#b7410e;">rust</span>"#);
        assert_eq!(style_of(&l, "rust").color, Some(0xb7410e));
    }

    #[test]
    fn a_br_is_a_hard_break() {
        let l = inline("a<br/>b");
        assert!(
            l.iter().any(|x| matches!(x, Leaf::Break { hard: true, .. })),
            "{l:?}"
        );
    }

    #[test]
    fn a_comment_disappears_from_the_rendered_text() {
        let l = inline("before<!-- not shown -->after");
        let joined: String = l
            .iter()
            .map(|x| x.plain(&mut String::new()).to_string())
            .collect();
        assert_eq!(joined, "beforeafter");
    }

    #[test]
    fn an_unknown_tag_is_still_shown_as_code() {
        // Anything we cannot act on stays visible: silently deleting markup
        // would eat text the author meant to keep.
        let l = inline("<widget>x</widget>");
        assert!(style_of(&l, "<widget>").code, "{l:?}");
    }

    #[test]
    fn an_inline_image_becomes_an_image() {
        let l = inline(r#"see <img src="a.png" alt="pic"> here"#);
        assert!(
            l.iter().any(|x| matches!(x, Leaf::Image { url, alt, .. } if url == "a.png" && alt == "pic")),
            "{l:?}"
        );
    }

    #[test]
    fn markup_mode_keeps_the_tags_visible() {
        // The editor draws the source, so the caret has to be able to walk over
        // every byte of it.
        let defs = HashMap::new();
        let src = "<b>bold</b> tail";
        let leaves = parse_inline(
            src,
            &InlineCtx {
                defs: &defs,
                keep_markers: true,
            },
        );
        let total: usize = leaves.iter().map(|l| l.src().len()).sum();
        assert_eq!(total, src.len());
        let joined: String = leaves
            .iter()
            .map(|x| x.plain(&mut String::new()).to_string())
            .collect();
        assert_eq!(joined, src);
    }
}
