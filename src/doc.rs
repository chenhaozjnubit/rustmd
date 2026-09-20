//! The document model: text, parsed block structure, undo history and file I/O.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::parser::{self, BlockKind, Parsed};

#[derive(Clone)]
struct Snapshot {
    text: String,
    cursor: usize,
    sel: usize,
}

/// Undo entries closer together than this are merged into one step.
const COALESCE: Duration = Duration::from_millis(650);
const MAX_UNDO: usize = 500;

pub struct Document {
    pub text: String,
    pub parsed: Arc<Parsed>,
    pub path: Option<PathBuf>,
    pub dirty: bool,

    /// Counts for the status bar.
    ///
    /// Kept here rather than worked out on demand: the status bar asks for them
    /// every frame, and counting characters and words across a ten megabyte
    /// document costs more than the rest of the frame put together.
    stats: Stats,

    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Option<Instant>,
    /// Caret position when the last snapshot was taken, so that a caret jump
    /// breaks an undo run.
    last_cursor: usize,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub chars: usize,
    pub chars_keep: usize,
    pub words: usize,
    pub lines: usize,
    pub paragraphs: usize,
    pub reading_minutes: f32,
}

#[derive(Debug, Clone)]
pub struct OutlineItem {
    pub level: u8,
    pub text: String,
    pub offset: usize,
    pub block: usize,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        let text = String::new();
        let parsed = Arc::new(parser::parse(&text));
        Self {
            stats: count_stats(&text, &parsed),
            text,
            parsed,
            path: None,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            last_cursor: 0,
        }
    }

    pub fn from_text(text: impl Into<String>, path: Option<PathBuf>) -> Self {
        let text = text.into();
        let parsed = Arc::new(parser::parse(&text));
        Self {
            stats: count_stats(&text, &parsed),
            text,
            parsed,
            path,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            last_cursor: 0,
        }
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        Ok(Self::from_text(text, Some(path.to_path_buf())))
    }

    pub fn reparse(&mut self) {
        self.parsed = Arc::new(parser::parse(&self.text));
        self.stats = count_stats(&self.text, &self.parsed);
    }

    /// Push an undo checkpoint. `self.text` must still hold the pre-edit state.
    fn snapshot(&mut self, cursor: usize, sel: usize, structural: bool) {
        let now = Instant::now();
        let close_in_time = self
            .last_edit
            .map(|t| now.duration_since(t) < COALESCE)
            .unwrap_or(false);
        let close_in_space = cursor.abs_diff(self.last_cursor) <= 1;
        let can_merge = close_in_time && close_in_space && !structural && !self.undo.is_empty();
        if !can_merge {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                cursor,
                sel,
            });
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_edit = Some(now);
        self.last_cursor = cursor;
    }

    /// Replace one block's source range and re-parse.
    pub fn apply_block_edit(
        &mut self,
        range: std::ops::Range<usize>,
        new_block: &str,
        cursor: usize,
        sel: usize,
    ) {
        if self.text.get(range.clone()) == Some(new_block) {
            return;
        }
        let old = self.text.get(range.clone()).unwrap_or("");
        let structural = old.contains('\n') != new_block.contains('\n')
            || new_block.contains('\n')
            || old.is_empty()
            || new_block.is_empty();
        self.snapshot(cursor, sel, structural);
        self.text.replace_range(range, new_block);
        self.reparse();
        self.dirty = true;
    }

    /// Insert a literal string at `at` (used for structural operations).
    pub fn insert_at(&mut self, at: usize, s: &str, cursor: usize) {
        let at = at.min(self.text.len());
        self.snapshot(cursor, cursor, true);
        self.text.insert_str(at, s);
        self.reparse();
        self.dirty = true;
    }

    /// Delete a byte range.
    pub fn delete_range(&mut self, range: std::ops::Range<usize>, cursor: usize) {
        let start = range.start.min(self.text.len());
        let end = range.end.min(self.text.len());
        if start >= end {
            return;
        }
        self.snapshot(cursor, cursor, true);
        self.text.replace_range(start..end, "");
        self.reparse();
        self.dirty = true;
    }

    pub fn undo(&mut self) -> Option<(usize, usize)> {
        let snap = self.undo.pop()?;
        let current = Snapshot {
            text: std::mem::replace(&mut self.text, snap.text),
            cursor: snap.cursor,
            sel: snap.sel,
        };
        self.redo.push(current);
        self.reparse();
        self.dirty = true;
        self.last_edit = None;
        Some((snap.cursor, snap.sel))
    }

    pub fn redo(&mut self) -> Option<(usize, usize)> {
        let snap = self.redo.pop()?;
        let current = Snapshot {
            text: std::mem::replace(&mut self.text, snap.text),
            cursor: snap.cursor,
            sel: snap.sel,
        };
        self.undo.push(current);
        self.reparse();
        self.dirty = true;
        self.last_edit = None;
        Some((snap.cursor, snap.sel))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // ------------------------------------------------------------------ files

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => "未命名.md".into(),
        }
    }

    pub fn dir(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
    }

    pub fn save(&mut self) -> std::io::Result<()> {
        match self.path.clone() {
            Some(p) => self.save_as(&p),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no path",
            )),
        }
    }

    pub fn save_as(&mut self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        std::fs::write(path, self.text.as_bytes())?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    // ------------------------------------------------------------------ stats

    /// Counts for the status bar, kept up to date by every edit.
    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn outline(&self) -> Vec<OutlineItem> {
        self.parsed
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(i, b)| match &b.kind {
                BlockKind::Heading { level } => Some(OutlineItem {
                    level: *level,
                    text: b.content.trim().to_string(),
                    offset: b.range.start,
                    block: i,
                }),
                _ => None,
            })
            .collect()
    }

    /// Index of the block containing (or preceding) `offset`.
    ///
    /// Blocks tile the document in order, so their ends are sorted and this is
    /// a search rather than a walk — which matters, because the status bar asks
    /// once per frame and a walk over a hundred thousand blocks would not be
    /// free.
    pub fn block_at(&self, offset: usize) -> usize {
        let blocks = &self.parsed.blocks;
        if blocks.is_empty() {
            return 0;
        }
        let i = blocks.partition_point(|b| b.range.end <= offset);
        i.min(blocks.len() - 1)
    }

    /// Byte offset of the start of the line containing `offset`.
    pub fn line_start(&self, offset: usize) -> usize {
        let o = offset.min(self.text.len());
        self.text[..o].rfind('\n').map(|p| p + 1).unwrap_or(0)
    }

    pub fn line_end(&self, offset: usize) -> usize {
        let o = offset.min(self.text.len());
        self.text[o..]
            .find('\n')
            .map(|p| o + p)
            .unwrap_or(self.text.len())
    }

    /// Markers-only prefix length for a given line, used by the editor to
    /// decide whether Backspace should delete structure or a character.
    pub fn marker_only_prefix(&self, offset: usize) -> bool {
        let idx = self.block_at(offset);
        let b = &self.parsed.blocks[idx];
        let ls = self.line_start(offset);
        let line = &self.text[ls..self.line_end(offset)];
        let line_no = self.text[..ls].matches('\n').count()
            - self.text[..b.range.start].matches('\n').count();
        let m = parser::line_marker_len(&b.kind, line_no, line);
        let ls_rel = b.range.start;
        let lead = ls.saturating_sub(ls_rel);
        offset <= ls + m && offset >= ls + lead
    }
}

// ===========================================================================
// Word counting (CJK aware)
// ===========================================================================

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF      // kana
        | 0x3400..=0x4DBF    // CJK ext A
        | 0x4E00..=0x9FFF    // CJK unified
        | 0xF900..=0xFAFF    // compatibility
        | 0x20000..=0x2FA1F  // ext B+
        | 0xAC00..=0xD7AF    // hangul
    )
}

/// Everything the status bar shows, worked out in one pass over the document.
fn count_stats(text: &str, parsed: &Parsed) -> Stats {
    let mut s = Stats::default();
    s.chars = text.chars().count();
    s.chars_keep = text.chars().filter(|c| !c.is_whitespace()).count();
    s.lines = text.lines().count().max(1);
    s.paragraphs = parsed
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::Paragraph))
        .count();
    s.words = count_words(text);
    // 300 words per minute for Latin, 400 characters per minute for CJK.
    let latin = count_latin_words(text) as f32;
    let cjk = count_cjk(text) as f32;
    s.reading_minutes = (latin / 300.0) + (cjk / 400.0);
    s
}

fn count_latin_words(text: &str) -> usize {
    let mut n = 0;
    let mut in_word = false;
    for c in text.chars() {
        if c.is_alphanumeric() && !is_cjk(c) {
            if !in_word {
                n += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    n
}

pub fn count_cjk(text: &str) -> usize {
    text.chars().filter(|c| is_cjk(*c)).count()
}

/// Words = Latin word runs + one per CJK character.
pub fn count_words(text: &str) -> usize {
    count_latin_words(text) + count_cjk(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_counting() {
        assert_eq!(count_words("hello world"), 2);
        assert_eq!(count_words("你好世界"), 4);
        assert_eq!(count_words("hello 世界"), 3);
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut d = Document::from_text("hello", None);
        d.insert_at(5, " world", 5);
        assert_eq!(d.text, "hello world");
        d.undo();
        assert_eq!(d.text, "hello");
        d.redo();
        assert_eq!(d.text, "hello world");
    }

    #[test]
    fn block_edit_keeps_document_consistent() {
        let mut d = Document::from_text("# a\n\ntext here\n", None);
        let idx = d.parsed.blocks.len() - 2; // the paragraph
        let range = d.parsed.blocks[idx].edit_range(&d.text);
        let new = "text here changed";
        d.apply_block_edit(range, new, 0, 0);
        assert!(d.text.contains("text here changed"));
        let rebuilt: String = d
            .parsed
            .blocks
            .iter()
            .map(|b| &d.text[b.range.clone()])
            .collect();
        assert_eq!(rebuilt, d.text);
    }
}
