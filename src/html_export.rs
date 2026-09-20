//! Standalone HTML export.
//!
//! Walks the same parsed block list the editor renders, so what is exported is
//! exactly what is on screen. The result is a single self-contained file: styles
//! are inlined and formula support is pulled from a CDN so the page degrades to
//! readable TeX when opened offline.

use crate::parser::{self, Align, BlockKind, InlineCtx, Leaf, Style};

/// Render `text` as a complete HTML document.
pub fn to_html(text: &str, title: &str, dark: bool) -> String {
    let parsed = parser::parse(text);
    let mut h = Builder::new(&parsed);

    for (i, b) in parsed.blocks.iter().enumerate() {
        h.quote(b.quote);
        match &b.kind {
            BlockKind::Blank => {}
            BlockKind::Paragraph => {
                h.list_depth(0);
                h.open("p");
                h.inline(&b.content);
                h.close();
            }
            BlockKind::Heading { level } => {
                h.list_depth(0);
                let tag = format!("h{}", (*level).clamp(1, 6));
                h.open(&tag);
                h.inline(&b.content);
                h.close();
            }
            BlockKind::ListItem(li) => {
                h.item_open(li.depth, li.ordered);
                if let Some(done) = li.task {
                    h.out.push_str("<input type=\"checkbox\" disabled");
                    if done {
                        h.out.push_str(" checked");
                    }
                    h.out.push_str("> ");
                }
                h.inline(&b.content);
            }
            BlockKind::Code(c) => {
                h.list_depth(0);
                h.ensure_closed();
                // A Mermaid fence goes out as a bare `<pre class="mermaid">` for
                // mermaid.js to pick up. Note that the export leans on the CDN
                // renderer, which knows more diagram types than the in-app
                // engine does — so a diagram can render in the exported file and
                // still fall back to a code block in the preview window.
                if crate::mermaid::is_mermaid(&c.lang) {
                    h.out.push_str("<pre class=\"mermaid\">");
                    h.out.push_str(&escape(&b.content));
                    h.out.push_str("</pre>\n");
                    continue;
                }
                h.out.push_str("<pre><code");
                if !c.lang.is_empty() {
                    h.out.push_str(" class=\"language-");
                    h.out.push_str(&esc_attr(&c.lang));
                    h.out.push('"');
                }
                h.out.push('>');
                let body = code_body(&b.content, c.fenced);
                h.out.push_str(&escape(&body));
                h.out.push_str("</code></pre>\n");
            }
            BlockKind::Table { aligns, rows, header_rows } => {
                h.list_depth(0);
                h.ensure_closed();
                h.out.push_str("<table>");
                for (ri, row) in rows.iter().enumerate() {
                    if ri == 0 && *header_rows > 0 {
                        h.out.push_str("<thead>");
                    }
                    if ri == *header_rows && *header_rows > 0 {
                        h.out.push_str("</thead><tbody>");
                    }
                    h.out.push_str("<tr>");
                    let tag = if ri < *header_rows { "th" } else { "td" };
                    for (ci, cell) in row.iter().enumerate() {
                        let align = aligns.get(ci).copied().unwrap_or(Align::None);
                        let style = match align {
                            Align::Center => " style=\"text-align:center\"",
                            Align::Right => " style=\"text-align:right\"",
                            _ => "",
                        };
                        h.out.push('<');
                        h.out.push_str(tag);
                        h.out.push_str(style);
                        h.out.push('>');
                        let raw = b.content.get(cell.clone()).unwrap_or("").trim().to_string();
                        h.inline(&raw);
                        h.out.push_str("</");
                        h.out.push_str(tag);
                        h.out.push('>');
                    }
                    h.out.push_str("</tr>");
                }
                if *header_rows > 0 {
                    h.out.push_str("</tbody>");
                }
                h.out.push_str("</table>\n");
            }
            BlockKind::Rule => {
                h.list_depth(0);
                h.ensure_closed();
                h.out.push_str("<hr>\n");
            }
            BlockKind::Math { .. } => {
                h.list_depth(0);
                h.ensure_closed();
                let body = math_body(&b.content);
                h.out.push_str("<div class=\"math-display\">\\[");
                h.out.push_str(&escape(&body));
                h.out.push_str("\\]</div>\n");
            }
            BlockKind::Html => {
                // Already HTML: pass it through untouched.
                h.list_depth(0);
                h.ensure_closed();
                h.out.push_str(&b.content);
                h.out.push('\n');
            }
            BlockKind::LinkDef { .. } | BlockKind::FootnoteDef { .. } => {}
        }
        let _ = i;
    }
    h.list_depth(0);
    h.quote(0);
    h.footnotes();

    let title = if title.trim().is_empty() {
        "未命名".to_string()
    } else {
        title.to_string()
    };

    let mut doc = String::with_capacity(h.out.len() + 8192);
    doc.push_str("<!DOCTYPE html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n");
    doc.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    doc.push_str("<title>");
    doc.push_str(&escape(&title));
    doc.push_str("</title>\n<style>\n");
    doc.push_str(&stylesheet(dark));
    doc.push_str("\n</style>\n");
    doc.push_str(
        "<script>\nwindow.MathJax={tex:{inlineMath:[['\\\\(','\\\\)']],displayMath:[['\\\\[','\\\\]']],processEscapes:true},options:{skipHtmlTags:['script','noscript','style','textarea','pre','code']}};\n</script>\n\
         <script id=\"MathJax-script\" async src=\"https://cdn.jsdelivr.net/npm/mathjax@3/es5/tex-mml-chtml.js\"></script>\n",
    );
    // Mermaid is loaded as a module because that is the only build the CDN
    // serves for v11, and it is given the same treatment as MathJax: the export
    // needs a network connection for either to draw anything.
    doc.push_str(
        "<script type=\"module\">\nimport mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@11/dist/mermaid.esm.min.mjs';\n\
         mermaid.initialize({ startOnLoad: true, securityLevel: 'strict' });\n</script>\n",
    );
    doc.push_str("</head>\n<body>\n<main class=\"page\">\n");
    doc.push_str(&h.out);
    doc.push_str("\n</main>\n</body>\n</html>\n");
    doc
}

// ===========================================================================
// Builder
// ===========================================================================

struct Builder<'a> {
    out: String,
    parsed: &'a parser::Parsed,
    quote: usize,
    /// Currently open `<ul>`/`<ol>` nesting: `true` = ordered.
    lists: Vec<bool>,
    /// Whether each open list level has an `<li>` waiting to be closed, so that
    /// a nested sub-list lands inside its parent item.
    items: Vec<bool>,
    open_tag: Option<String>,
}

impl<'a> Builder<'a> {
    fn new(parsed: &'a parser::Parsed) -> Self {
        Self {
            out: String::new(),
            parsed,
            quote: 0,
            lists: Vec::new(),
            items: Vec::new(),
            open_tag: None,
        }
    }

    /// Keep the blockquote nesting in sync with the block's depths.
    fn quote(&mut self, depth: usize) {
        self.ensure_closed();
        while self.quote > depth {
            self.out.push_str("</blockquote>\n");
            self.quote -= 1;
        }
        while self.quote < depth {
            self.out.push_str("<blockquote>\n");
            self.quote += 1;
        }
    }

    /// Open a block-level element, closing any block still running.
    fn open(&mut self, tag: &str) {
        self.ensure_closed();
        self.out.push('<');
        self.out.push_str(tag);
        self.out.push('>');
        self.open_tag = Some(tag.to_string());
    }

    /// Close the block opened by [`Builder::open`].
    fn close(&mut self) {
        if let Some(tag) = self.open_tag.take() {
            self.out.push_str("</");
            self.out.push_str(&tag);
            self.out.push_str(">\n");
        }
    }

    /// Close whatever block is currently open, if any.
    fn ensure_closed(&mut self) {
        self.close();
    }

    /// Close open lists until at most `depth` levels remain open.
    fn list_depth(&mut self, depth: usize) {
        while self.lists.len() > depth {
            self.close_li();
            self.close_list();
        }
    }

    fn item_open(&mut self, depth: usize, ordered: bool) {
        self.ensure_closed();

        // Close list levels deeper than this item.
        while self.lists.len() > depth + 1 {
            self.close_li();
            self.close_list();
        }
        // At the level this item lives on: close the previous sibling, and
        // restart the list when the kind changed (`-` run then `1.` run).
        if self.lists.len() == depth + 1 {
            self.close_li();
            if self.lists[depth] != ordered {
                self.close_list();
            }
        }
        // Descending leaves the parent `<li>` open, so a nested list is emitted
        // inside its parent item.
        while self.lists.len() < depth + 1 {
            self.out.push_str(if ordered { "<ol>\n" } else { "<ul>\n" });
            self.lists.push(ordered);
            self.items.push(false);
        }
        self.out.push_str("<li>");
        if let Some(open) = self.items.last_mut() {
            *open = true;
        }
    }

    /// Close the innermost open `<li>`, if there is one.
    fn close_li(&mut self) {
        if let Some(open) = self.items.last_mut() {
            if *open {
                self.out.push_str("</li>\n");
                *open = false;
            }
        }
    }

    fn close_list(&mut self) {
        self.items.pop();
        let ordered = self.lists.pop().unwrap_or(false);
        self.out
            .push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
    }

    fn footnotes(&mut self) {
        if self.parsed.footnotes.is_empty() {
            return;
        }
        self.ensure_closed();
        self.list_depth(0);
        self.out.push_str("<hr class=\"footnotes-sep\">\n<ol class=\"footnotes\">\n");
        for (name, body) in &self.parsed.footnotes {
            self.out.push_str("<li id=\"fn-");
            self.out.push_str(&esc_attr(name));
            self.out.push_str("\">");
            self.inline(body);
            self.out.push_str("</li>\n");
        }
        self.out.push_str("</ol>\n");
    }

    // -------------------------------------------------------------- inline

    fn inline(&mut self, src: &str) {
        let ctx = InlineCtx {
            defs: &self.parsed.defs,
            keep_markers: false,
        };
        let leaves = parser::parse_inline(src, &ctx);
        for leaf in &leaves {
            self.leaf(leaf);
        }
    }

    fn leaf(&mut self, leaf: &Leaf) {
        match leaf {
            Leaf::Span { text, style, link, .. } => {
                let html = self.span(text, *style);
                match link {
                    Some(url) => {
                        self.out.push_str("<a href=\"");
                        self.out.push_str(&esc_attr(url));
                        self.out.push_str("\"");
                        if !url.starts_with('#') {
                            self.out.push_str(" target=\"_blank\" rel=\"noreferrer\"");
                        }
                        self.out.push('>');
                        self.out.push_str(&html);
                        self.out.push_str("</a>");
                    }
                    None => self.out.push_str(&html),
                }
            }
            Leaf::Image { alt, url, .. } => {
                self.out.push_str("<img src=\"");
                self.out.push_str(&esc_attr(url));
                self.out.push_str("\" alt=\"");
                self.out.push_str(&esc_attr(alt));
                self.out.push_str("\">");
            }
            Leaf::Math { tex, display, .. } => {
                if *display {
                    self.out.push_str("<span class=\"math-display\">\\[");
                    self.out.push_str(&escape(tex));
                    self.out.push_str("\\]</span>");
                } else {
                    self.out.push_str("<span class=\"math-inline\">\\(");
                    self.out.push_str(&escape(tex));
                    self.out.push_str("\\)</span>");
                }
            }
            Leaf::Break { hard, .. } => {
                self.out.push_str(if *hard { "<br>\n" } else { " " });
            }
            Leaf::Footnote { name, .. } => {
                self.out.push_str("<sup class=\"fn-ref\"><a href=\"#fn-");
                self.out.push_str(&esc_attr(name));
                self.out.push_str("\">[");
                self.out.push_str(&escape(name));
                self.out.push_str("]</a></sup>");
            }
        }
    }

    fn span(&mut self, text: &str, style: Style) -> String {
        let mut s = escape(text);
        if style.code {
            return format!("<code>{s}</code>");
        }
        if style.highlight {
            s = format!("<mark>{s}</mark>");
        }
        if style.strike {
            s = format!("<del>{s}</del>");
        }
        if style.em {
            s = format!("<em>{s}</em>");
        }
        if style.strong {
            s = format!("<strong>{s}</strong>");
        }
        s
    }
}

/// The raw lines of a fenced code block, with the fence lines removed.
fn code_body(content: &str, fenced: bool) -> String {
    if !fenced {
        return content.trim_end_matches('\n').to_string();
    }
    let mut lines: Vec<&str> = content.lines().collect();
    if !lines.is_empty() {
        let first = lines[0].trim_start();
        if first.starts_with("```") || first.starts_with("~~~") {
            lines.remove(0);
        }
    }
    if let Some(last) = lines.last() {
        let t = last.trim();
        if t.len() >= 3 && (t.chars().all(|c| c == '`') || t.chars().all(|c| c == '~')) {
            lines.pop();
        }
    }
    lines.join("\n")
}

/// Pull the TeX out of a `$$ … $$` block.
fn math_body(content: &str) -> String {
    let t = content.trim();
    let t = t.strip_prefix("$$").unwrap_or(t);
    let t = t.strip_suffix("$$").unwrap_or(t);
    t.trim().to_string()
}

// ===========================================================================
// Text helpers
// ===========================================================================

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn esc_attr(s: &str) -> String {
    escape(s).replace('"', "&quot;")
}

// ===========================================================================
// Stylesheet
// ===========================================================================

fn stylesheet(dark: bool) -> String {
    let (bg, fg, muted, border, code_bg, accent, quote_bar, head_bg) = if dark {
        (
            "#1C1F25", "#D5DBE3", "#8B94A1", "#2B3038", "#14171B", "#6CB6FF", "#3B434D", "#242931",
        )
    } else {
        (
            "#FFFFFF", "#262B33", "#6E7681", "#E4E8ED", "#F6F8FA", "#2F6FEB", "#D3D9E0", "#F4F6F8",
        )
    };
    format!(
        r#":root {{
  --bg: {bg}; --fg: {fg}; --muted: {muted}; --border: {border};
  --code-bg: {code_bg}; --accent: {accent}; --quote-bar: {quote_bar}; --head-bg: {head_bg};
}}
* {{ box-sizing: border-box; }}
html {{ -webkit-text-size-adjust: 100%; }}
body {{
  margin: 0; background: var(--bg); color: var(--fg);
  font-family: -apple-system, "PingFang SC", "Helvetica Neue", "Microsoft YaHei", sans-serif;
  font-size: 16.5px; line-height: 1.72;
  -webkit-font-smoothing: antialiased;
}}
.page {{ max-width: 780px; margin: 0 auto; padding: 56px 28px 120px; }}
h1, h2, h3, h4, h5, h6 {{ line-height: 1.3; margin: 1.6em 0 .6em; font-weight: 600; }}
h1 {{ font-size: 1.85em; margin-top: 1.1em; }}
h2 {{ font-size: 1.48em; padding-bottom: .24em; border-bottom: 1px solid var(--border); }}
h3 {{ font-size: 1.24em; }}
h4 {{ font-size: 1.08em; }}
h5, h6 {{ font-size: 1em; color: var(--muted); }}
p {{ margin: .85em 0; }}
a {{ color: var(--accent); text-decoration: none; }}
a:hover {{ text-decoration: underline; }}
strong {{ font-weight: 600; }}
del {{ color: var(--muted); }}
mark {{ background: #FFF3B0; color: #4A3B00; padding: 0 .18em; border-radius: 3px; }}
hr {{ border: none; border-top: 1px solid var(--border); margin: 2em 0; }}
blockquote {{
  margin: 1em 0; padding: .1em 0 .1em 1em;
  border-left: 3px solid var(--quote-bar); color: var(--muted);
}}
blockquote p {{ margin: .5em 0; }}
code {{
  font-family: "SF Mono", Menlo, Consolas, monospace; font-size: .88em;
  background: var(--code-bg); padding: .13em .38em; border-radius: 4px;
}}
pre {{
  background: var(--code-bg); border: 1px solid var(--border); border-radius: 8px;
  padding: 14px 16px; overflow-x: auto; margin: 1.1em 0; line-height: 1.55;
}}
pre code {{ background: none; padding: 0; font-size: .86em; }}
ul, ol {{ margin: .7em 0; padding-left: 1.6em; }}
li {{ margin: .25em 0; }}
li > ul, li > ol {{ margin: .25em 0; }}
input[type=checkbox] {{ margin-right: .35em; }}
table {{ border-collapse: collapse; margin: 1.2em 0; width: 100%; font-size: .95em; }}
th, td {{ border: 1px solid var(--border); padding: .5em .7em; text-align: left; }}
th {{ background: var(--head-bg); font-weight: 600; }}
img {{ max-width: 100%; border-radius: 6px; }}
.math-display {{ text-align: center; margin: 1.2em 0; overflow-x: auto; }}
.footnotes {{ font-size: .92em; color: var(--muted); }}
.footnotes-sep {{ margin-top: 3em; }}
@media print {{
  body {{ background: #fff; }}
  .page {{ max-width: none; padding: 0; }}
  pre, table, blockquote {{ break-inside: avoid; }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_headings_and_emphasis() {
        let html = to_html("# Title\n\nsome **bold** and `code`\n", "t", false);
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<code>code</code>"));
        assert!(html.starts_with("<!DOCTYPE html>"));
    }

    #[test]
    fn exports_nested_lists() {
        let html = to_html("- a\n  - b\n- c\n", "t", false);
        assert!(html.contains("<ul>"));
        assert_eq!(html.matches("<li>").count(), 3);
        assert_eq!(html.matches("</ul>").count(), 2);
        assert_eq!(html.matches("</li>").count(), 3);
        // The sub-list must live inside its parent item, not next to it.
        assert!(html.contains("<li>a<ul>"), "{html}");
        assert!(html.contains("<li>b</li>"), "{html}");
    }

    #[test]
    fn restarts_numbering_when_the_list_kind_changes() {
        let html = to_html("- a\n1. b\n", "t", false);
        assert!(html.contains("<ul>\n<li>a</li>\n</ul>"), "{html}");
        assert!(html.contains("<ol>\n<li>b</li>\n</ol>"), "{html}");
    }

    #[test]
    fn exports_code_and_table() {
        let md = "```rust\nfn main() {}\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
        let html = to_html(md, "t", false);
        assert!(html.contains("<pre><code class=\"language-rust\">fn main() {}"));
        assert!(html.contains("<th>a</th>"));
        assert!(html.contains("<td>1</td>"));
    }

    #[test]
    fn escapes_html_in_text() {
        let html = to_html("a < b & c\n", "t", false);
        assert!(html.contains("a &lt; b &amp; c"));
    }

    #[test]
    fn renders_math() {
        let html = to_html("$$\nx^2\n$$\n", "t", false);
        assert!(html.contains("\\(x^2\\)") || html.contains("\\[x^2\\]"));
    }
}
