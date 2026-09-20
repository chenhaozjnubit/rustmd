//! Syntax highlighting for fenced code blocks, backed by `syntect` with its
//! bundled syntax and theme dumps (no external assets to ship).
//!
//! Highlighting is expensive relative to everything else in the render path, so
//! results are memoised in a small bounded cache keyed by (language, theme,
//! source hash).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use egui::Color32;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub color: Color32,
    pub italic: bool,
    pub bold: bool,
}


struct Engine {
    syntaxes: SyntaxSet,
    themes: ThemeSet,
}

fn engine() -> &'static Engine {
    static E: OnceLock<Engine> = OnceLock::new();
    E.get_or_init(|| Engine {
        syntaxes: SyntaxSet::load_defaults_newlines(),
        themes: ThemeSet::load_defaults(),
    })
}

type Cache = Mutex<HashMap<(u64, String), Vec<Vec<Token>>>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hash(code: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    h.finish()
}

/// Highlight `code` as `lang`. Returns one `Vec<Token>` per source line.
/// `default_color` is used when the theme provides no foreground, and for the
/// plain-text fallback.
pub fn highlight(code: &str, lang: &str, theme_name: &str, default_color: Color32) -> Vec<Vec<Token>> {
    let key = (hash(code), format!("{lang}\u{1}{theme_name}"));
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return hit.clone();
        }
    }
    let out = highlight_uncached(code, lang, theme_name, default_color);
    if let Ok(mut c) = cache().lock() {
        if c.len() > 128 {
            c.clear();
        }
        c.insert(key, out.clone());
    }
    out
}

fn to_color(c: syntect::highlighting::Color, fallback: Color32) -> Color32 {
    if c.a == 0 {
        return fallback;
    }
    Color32::from_rgb(c.r, c.g, c.b)
}

fn highlight_uncached(
    code: &str,
    lang: &str,
    theme_name: &str,
    default_color: Color32,
) -> Vec<Vec<Token>> {
    let e = engine();
    let theme = e
        .themes
        .themes
        .get(theme_name)
        .or_else(|| e.themes.themes.get("InspiredGitHub"));

    let syntax = e
        .syntaxes
        .find_syntax_by_token(lang)
        .or_else(|| {
            if lang.is_empty() {
                None
            } else {
                e.syntaxes.find_syntax_by_extension(lang)
            }
        })
        .or_else(|| {
            if lang.is_empty() {
                None
            } else {
                e.syntaxes.find_syntax_by_name(lang)
            }
        })
        .unwrap_or_else(|| e.syntaxes.find_syntax_plain_text());

    let Some(theme) = theme else {
        return fallback_plain(code, default_color);
    };

    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = match h.highlight_line(line, &e.syntaxes) {
            Ok(r) => r,
            Err(_) => return fallback_plain(code, default_color),
        };
        let mut toks = Vec::new();
        for (style, text) in ranges {
            let text = text.trim_end_matches(['\n', '\r']);
            if text.is_empty() {
                continue;
            }
            toks.push(Token {
                text: text.to_string(),
                color: to_color(style.foreground, default_color),
                italic: style.font_style.contains(FontStyle::ITALIC),
                bold: style.font_style.contains(FontStyle::BOLD),
            });
        }
        out.push(toks);
    }
    // `LinesWithEndings` never yields a final empty line for trailing \n; make
    // sure the number of lines matches the source.
    let want = code.split('\n').count();
    while out.len() < want {
        out.push(Vec::new());
    }
    out.truncate(want);
    out
}

fn fallback_plain(code: &str, color: Color32) -> Vec<Vec<Token>> {
    code.split('\n')
        .map(|l| {
            if l.is_empty() {
                Vec::new()
            } else {
                vec![Token {
                    text: l.to_string(),
                    color,
                    italic: false,
                    bold: false,
                }]
            }
        })
        .collect()
}

/// Longest source line (in characters), used to size the horizontal scroll area
/// of a non-wrapping code block.
pub fn max_line_chars(code: &str) -> usize {
    code.split('\n').map(|l| l.chars().count()).max().unwrap_or(0)
}

/// A single accent colour per language for the little badge on code blocks.
pub fn lang_accent(lang: &str, dark: bool) -> Color32 {
    let (r, g, b) = match lang {
        "rust" => (222, 165, 132),
        "python" => (255, 212, 59),
        "js" | "javascript" | "jsx" => (247, 223, 30),
        "ts" | "typescript" | "tsx" => (49, 120, 198),
        "c" | "cpp" | "c++" | "h" | "hpp" => (0, 89, 156),
        "go" => (0, 173, 216),
        "java" => (237, 41, 57),
        "sh" | "bash" | "zsh" | "shell" => (137, 224, 81),
        "json" => (203, 203, 203),
        "yaml" | "yml" => (231, 119, 72),
        "html" | "xml" => (228, 79, 38),
        "css" | "scss" => (86, 61, 124),
        "sql" => (0, 122, 204),
        "toml" => (156, 66, 33),
        _ => (140, 140, 140),
    };
    let f = if dark { 1.0 } else { 0.78 };
    Color32::from_rgb(
        (r as f32 * f) as u8,
        (g as f32 * f) as u8,
        (b as f32 * f) as u8,
    )
}
