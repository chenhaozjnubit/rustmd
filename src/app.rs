//! Application state and every document-level command.
//!
//! The [`App`] owns the document, the editor widget state, the persistent
//! configuration and the panels' data. `panels.rs` and `main.rs` only ever talk
//! to this type, so all mutating logic lives in one place.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{Align, Key, Modifiers, ScrollArea, TextEdit, Ui, Vec2};

use crate::config::Config;
use crate::doc::Document;
use crate::editor::{Editor, EditorCtx};
use crate::fonts::{self, FontReport};
use crate::html_export;
use crate::panels;
use crate::picker::{self, FilePicker};
use crate::theme::{Theme, ThemeMode};

/// How long a toast message stays on the status bar.
const TOAST: f32 = 4.0;

// ===========================================================================
// Small data types
// ===========================================================================

/// Which view of the document is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    /// Typora-style live rendering.
    Edit,
    /// Raw Markdown for the whole document.
    Source,
    /// Live rendering on the left, raw Markdown on the right.
    Split,
}

/// One entry of the file tree.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub children: Vec<TreeNode>,
}

/// Find & replace state.
#[derive(Debug, Default)]
pub struct FindState {
    pub open: bool,
    pub query: String,
    pub replace: String,
    /// Byte ranges of the hits in the document.
    pub matches: Vec<(usize, usize)>,
    pub current: usize,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub focus_query: bool,
}

/// What a button in a [`Prompt`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptChoice {
    /// Write the document, then carry on with the pending action.
    Save,
    /// Throw the changes away and carry on.
    Discard,
    /// Do nothing at all.
    Cancel,
    /// Acknowledge information.
    Ok,
}

/// A modal question with a fixed set of answers.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub title: String,
    pub body: String,
    pub choices: Vec<(String, PromptChoice)>,
}

impl Prompt {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            choices: vec![("确定".into(), PromptChoice::Ok)],
        }
    }

    pub fn choice(mut self, label: impl Into<String>, choice: PromptChoice) -> Self {
        if self.choices.len() == 1 && self.choices[0].1 == PromptChoice::Ok {
            self.choices.clear();
        }
        self.choices.push((label.into(), choice));
        self
    }
}

/// An action waiting for the unsaved-changes question to be answered.
#[derive(Debug, Clone)]
enum Pending {
    Quit,
    NewDoc,
    OpenPath(PathBuf),
}

// ===========================================================================
// App
// ===========================================================================

pub struct App {
    pub cfg: Config,
    pub theme: Theme,
    pub font_report: FontReport,

    pub doc: Document,
    pub editor: Editor,
    pub pane: Pane,

    pub tree_root: Option<PathBuf>,
    pub tree: Vec<TreeNode>,
    pub collapsed: HashSet<PathBuf>,

    pub find: FindState,
    pub toast: Option<(String, Instant)>,
    pub show_help: bool,
    pub show_about: bool,
    pub prompt: Option<Prompt>,
    /// The built-in file browser, used whenever the OS panel is unavailable,
    /// filtered or otherwise unhelpful.
    pub picker: FilePicker,
    /// Set while the pointer is carrying files over the window.
    pub drag_hint: Option<String>,

    /// Rebuilt every frame so commands can raise toasts and close the window.
    ctx: Option<egui::Context>,
    pending: Option<Pending>,
    /// Set once the user has agreed to lose unsaved changes.
    quit_ok: bool,
    applied_theme: ThemeMode,
    last_autosave: Instant,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_ctx(&cc.egui_ctx)
    }

    /// Build the app from a context alone.
    ///
    /// `eframe::CreationContext` is a thin wrapper around the `egui::Context`
    /// and nothing else here needs it, so the app can be built without
    /// `eframe` at all — which is what lets a test drive a real frame.
    pub fn with_ctx(ctx: &egui::Context) -> Self {
        Self::with_ctx_and_config(ctx, Config::load())
    }

    /// As [`App::with_ctx`], but with the settings handed in rather than read
    /// from disk, so a test neither depends on nor disturbs the user's config.
    pub fn with_ctx_and_config(ctx: &egui::Context, cfg: Config) -> Self {
        let theme = Theme::for_mode(cfg.theme);
        let font_report = fonts::install(ctx);
        theme.apply(ctx);

        // Arrange to receive documents Finder hands over. The real
        // registration happens when the executable is loaded (see
        // `macos::install`), because a cold-start `odoc` event is dispatched
        // before this constructor runs; calling it again here is a cheap
        // safety net for the already-running case.
        crate::macos::install();

        let mut app = Self {
            cfg,
            theme,
            font_report,
            doc: Document::new(),
            editor: Editor::default(),
            pane: Pane::Edit,
            tree_root: None,
            tree: Vec::new(),
            collapsed: HashSet::new(),
            find: FindState::default(),
            toast: None,
            show_help: false,
            show_about: false,
            prompt: None,
            picker: FilePicker::default(),
            drag_hint: None,
            ctx: None,
            pending: None,
            quit_ok: false,
            applied_theme: ThemeMode::Light,
            last_autosave: Instant::now(),
        };
        app.cfg.drop_missing_recent();
        app.applied_theme = app.cfg.theme;
        app.editor.set_caret(0, Some(0));
        app
    }

    /// Open a file given on the command line.
    pub fn open_startup_file(&mut self, path: &Path) {
        self.load_path(path);
    }

    // ------------------------------------------------------------------ pane

    pub fn pane(&self) -> Pane {
        self.pane
    }

    pub fn set_pane(&mut self, pane: Pane) {
        self.pane = pane;
        if pane == Pane::Edit {
            self.editor.reading = false;
        }
    }

    pub fn toggle_pane(&mut self) {
        self.set_pane(match self.pane {
            Pane::Edit => Pane::Source,
            Pane::Source => Pane::Split,
            Pane::Split => Pane::Edit,
        });
    }

    // -------------------------------------------------------------- document

    pub fn new_document(&mut self) {
        if self.doc.dirty {
            self.ask_unsaved(Pending::NewDoc);
            return;
        }
        self.reset_document();
    }

    fn reset_document(&mut self) {
        self.doc = Document::new();
        self.editor = Editor::default();
        self.editor.set_caret(0, Some(0));
        self.find.matches.clear();
        self.find.current = 0;
    }

    /// The primary way to open a document: our own browser.
    ///
    /// It filters to Markdown by default, can be told to show everything, and
    /// cannot be broken by an OS-level file-type filter.
    pub fn open_dialog(&mut self) {
        let start = self.start_dir();
        self.picker.open_at(start);
    }

    /// The OS file panel, kept as an alternative because it reaches places our
    /// own browser cannot: iCloud Drive, network volumes, the sidebar.
    ///
    /// Deliberately installed with **no content filter**. `rfd` drives the
    /// deprecated `NSSavePanel.setAllowedFileTypes`, and a panel whose allowed
    /// types fail to resolve greys out *every* file — which is precisely the
    /// "I cannot pick my .md file" failure. An unfiltered panel always lets you
    /// pick, and this program opens any text file happily anyway.
    pub fn open_system_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new().set_title("打开 Markdown");
        if let Some(d) = self.start_dir() {
            dlg = dlg.set_directory(d);
        }
        if let Some(p) = dlg.pick_file() {
            self.open_path(&p);
        }
    }

    /// Where a file/folder dialog should start.
    pub fn start_dir(&self) -> Option<PathBuf> {
        self.cfg
            .last_dir
            .clone()
            .or_else(|| self.doc.dir())
            .filter(|d| d.is_dir())
    }

    pub fn open_folder_dialog(&mut self) {
        let start = self.start_dir();
        self.picker.open_folder_at(start);
    }

    /// Point the sidebar tree at `dir`, as the picker's folder mode does.
    pub fn browse_folder(&mut self, dir: PathBuf) {
        self.tree_root = Some(dir.clone());
        self.cfg.last_dir = Some(dir.clone());
        self.cfg.save();
        self.refresh_tree();
        self.toast(format!("已打开文件夹 {}", file_label(&dir)));
    }

    // -------------------------------------------------------- file picker

    /// Open whatever the built-in picker has highlighted. A folder is entered
    /// rather than opened, which is what a double-click means everywhere else.
    pub fn accept_picker_file(&mut self) {
        let Some(e) = self.picker.selected() else {
            self.toast("没有可打开的文件");
            return;
        };
        if e.is_dir {
            self.picker.go(e.path);
            return;
        }
        self.picker.close();
        self.open_path(&e.path);
    }

    /// Take the picker's current directory as the sidebar root.
    pub fn accept_picker_folder(&mut self) {
        let dir = self.picker.dir.clone();
        self.picker.close();
        self.browse_folder(dir);
    }

    /// Open documents that Finder handed over.
    ///
    /// A bundled app is launched without the file in `argv`, so a double-click
    /// arrives as an Apple Event a moment after the window appears. Only the
    /// first file is opened: `open_path` can raise a "discard changes?" prompt,
    /// and only one of those can be pending.
    fn handle_finder_open(&mut self) {
        let opened = crate::macos::take_opened();
        let Some(first) = opened.first().cloned() else {
            return;
        };
        let extra = opened.len() - 1;
        self.open_path(&first);
        if extra > 0 {
            self.toast(format!("Finder 传入了 {} 个文件，已打开第一个", opened.len()));
        }
    }

    /// Consume any files dropped onto the window.
    ///
    /// A dropped folder becomes the sidebar root, a dropped file is opened.
    /// This is the fastest path from Finder to a rendered document and the one
    /// that most obviously *should* work in a Markdown reader.
    pub fn handle_drops(&mut self, ctx: &egui::Context) {
        self.drag_hint = ctx.input(|i| {
            i.raw
                .hovered_files
                .iter()
                .find_map(|f| f.path.as_ref())
                .map(|p| {
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string())
                })
        });

        let dropped: Vec<PathBuf> = ctx.input_mut(|i| {
            std::mem::take(&mut i.raw.dropped_files)
                .into_iter()
                .filter_map(|f| f.path)
                .collect()
        });
        if dropped.is_empty() {
            return;
        }

        let mut files: Vec<PathBuf> = Vec::new();
        for p in dropped {
            if p.is_dir() {
                self.browse_folder(p);
            } else {
                files.push(p);
            }
        }
        let Some(first) = files.first().cloned() else {
            return;
        };
        if files.len() > 1 {
            self.toast(format!("拖入了 {} 个文件，打开第一个", files.len()));
        }
        self.open_path(&first);
    }

    pub fn open_path(&mut self, path: &Path) {
        if path.is_dir() {
            self.tree_root = Some(path.to_path_buf());
            self.refresh_tree();
            return;
        }
        if self.doc.dirty && self.doc.path.as_deref() != Some(path) {
            self.ask_unsaved(Pending::OpenPath(path.to_path_buf()));
            return;
        }
        self.load_path(path);
    }

    fn load_path(&mut self, path: &Path) {
        match Document::open(path) {
            Ok(mut d) => {
                d.dirty = false;
                let start = 0usize;
                self.doc = d;
                self.editor = Editor::default();
                let b = self.doc.block_at(start);
                self.editor.set_caret(start, Some(b));
                self.find.matches.clear();
                self.find.current = 0;
                self.cfg.remember(path);
                self.set_tree_for(path);
                self.cfg.save();
                self.toast(format!("已打开 {}", file_label(path)));
            }
            Err(e) => self.toast(format!("打开失败：{e}")),
        }
    }

    /// Point the file tree at the new document's folder the first time.
    fn set_tree_for(&mut self, path: &Path) {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
            _ => return,
        };
        if self.tree_root.as_ref() != Some(&dir) {
            self.tree_root = Some(dir);
            self.refresh_tree();
        }
    }

    pub fn reload(&mut self) {
        let Some(path) = self.doc.path.clone() else {
            self.toast("尚未保存，无法重新载入");
            return;
        };
        match Document::open(&path) {
            Ok(mut d) => {
                d.dirty = false;
                let cursor = self.editor.cursor.min(d.text.len());
                self.doc = d;
                let b = self.doc.block_at(cursor);
                self.editor.set_caret(cursor, Some(b));
                self.toast("已重新载入");
            }
            Err(e) => self.toast(format!("重新载入失败：{e}")),
        }
    }

    pub fn save(&mut self) -> bool {
        let ok = if self.doc.path.is_none() {
            self.save_as_dialog()
        } else {
            match self.doc.save() {
                Ok(()) => {
                    if let Some(p) = self.doc.path.clone() {
                        self.cfg.remember(&p);
                        let name = file_label(&p);
                        self.toast(format!("已保存 {name}"));
                    }
                    true
                }
                Err(e) => {
                    self.toast(format!("保存失败：{e}"));
                    false
                }
            }
        };
        if ok {
            self.cfg.save();
            self.after_save();
        }
        ok
    }

    pub fn save_as_dialog(&mut self) -> bool {
        // Same reasoning as `open_system_dialog`: no content filter, so the
        // panel can never refuse the name the user typed. The extension is
        // defaulted in the name field and filled in afterwards if it is missing.
        let mut dlg = rfd::FileDialog::new()
            .set_title("另存为 Markdown")
            .set_file_name(self.doc.display_name());
        if let Some(d) = self.start_dir() {
            dlg = dlg.set_directory(d);
        }
        let Some(path) = dlg.save_file() else {
            return false;
        };
        let path = with_extension(path, "md");
        match self.doc.save_as(&path) {
            Ok(()) => {
                self.cfg.remember(&path);
                self.cfg.save();
                self.set_tree_for(&path);
                self.toast(format!("已保存 {}", file_label(&path)));
                true
            }
            Err(e) => {
                self.toast(format!("保存失败：{e}"));
                false
            }
        }
    }

    pub fn export_html_dialog(&mut self) {
        let stem = self.doc_stem();
        let mut dlg = rfd::FileDialog::new()
            .set_title("导出为 HTML")
            .set_file_name(format!("{stem}.html"));
        if let Some(d) = self.start_dir() {
            dlg = dlg.set_directory(d);
        }
        let Some(path) = dlg.save_file() else {
            return;
        };
        let path = with_extension(path, "html");
        let title = self.doc_stem();
        let html = html_export::to_html(&self.doc.text, &title, self.cfg.theme.is_dark());
        match std::fs::write(&path, html) {
            Ok(()) => self.toast(format!("已导出 {}", file_label(&path))),
            Err(e) => self.toast(format!("导出失败：{e}")),
        }
    }

    /// The document's name without its extension, for default file names.
    pub fn doc_stem(&self) -> String {
        self.doc
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "未命名".to_string())
    }

    pub fn copy_all(&mut self) {
        if let Some(ctx) = self.ctx.clone() {
            ctx.copy_text(self.doc.text.clone());
            self.toast("已复制全文");
        }
    }

    // ---------------------------------------------------------- edit actions

    pub fn undo(&mut self) {
        if let Some((cursor, sel)) = self.doc.undo() {
            let b = self.doc.block_at(cursor);
            self.editor.set_caret(cursor, Some(b));
            self.editor.sel = sel;
            self.editor.scroll_to_block = Some(b);
        } else {
            self.toast("没有可撤销的操作");
        }
    }

    pub fn redo(&mut self) {
        if let Some((cursor, sel)) = self.doc.redo() {
            let b = self.doc.block_at(cursor);
            self.editor.set_caret(cursor, Some(b));
            self.editor.sel = sel;
            self.editor.scroll_to_block = Some(b);
        } else {
            self.toast("没有可重做的操作");
        }
    }

    pub fn select_all(&mut self) {
        self.editor.select_all(&self.doc);
    }

    /// Wrap the selection in `open`/`close`, toggling the markers off if they
    /// are already there. With no selection the pair is inserted and the caret
    /// lands between the two halves.
    pub fn wrap_selection(&mut self, open: &str, close: &str) {
        let text = self.doc.text.clone();
        let (a, b) = (
            self.editor.sel.min(self.editor.cursor).min(text.len()),
            self.editor.sel.max(self.editor.cursor).min(text.len()),
        );
        if a == b {
            let ins = format!("{open}{close}");
            self.doc.insert_at(a, &ins, a);
            let c = a + open.len();
            let blk = self.doc.block_at(c);
            self.editor.set_caret(c, Some(blk));
            return;
        }

        let inner = text[a..b].to_string();
        let before = &text[..a];
        let after = &text[b..];
        if before.ends_with(open) && after.starts_with(close) {
            let from = a - open.len();
            let to = b + close.len();
            self.doc.apply_block_edit(from..to, &inner, from, from);
            let blk = self.doc.block_at(from);
            self.editor.set_caret(from, Some(blk));
            return;
        }
        let replaced = format!("{open}{inner}{close}");
        self.doc.apply_block_edit(a..b, &replaced, a, b);
        let blk = self.doc.block_at(a);
        self.editor.set_caret(a, Some(blk));
        self.editor.cursor = a + replaced.len();
        self.editor.scroll_to_block = Some(blk);
    }

    /// Add a prefix to every line touched by the selection.
    pub fn prefix_lines(&mut self, prefix: &str) {
        let len = self.doc.text.len();
        let a = self.editor.sel.min(self.editor.cursor).min(len);
        let b = self.editor.sel.max(self.editor.cursor).min(len);
        let start = self.doc.line_start(a);
        // Include the final line when the selection ends exactly on a break.
        let end = if b > a && b > 0 && self.doc.text.as_bytes().get(b - 1) == Some(&b'\n') {
            b - 1
        } else {
            b
        };
        let end = self.doc.line_end(end).max(start);

        let segment = self.doc.text[start..end].to_string();
        let already = segment
            .split('\n')
            .all(|l| l.is_empty() || l.starts_with(prefix));
        let mut out = String::with_capacity(segment.len() * 2);
        for (i, line) in segment.split('\n').enumerate() {
            if i > 0 {
                out.push('\n');
            }
            if already {
                // Toggling the same prefix off again.
                out.push_str(line.strip_prefix(prefix).unwrap_or(line));
            } else {
                out.push_str(prefix);
                out.push_str(line);
            }
        }
        if already {
            // A prefix may have been removed from the very start of the caret's line.
            let removed = if segment.split('\n').next().unwrap_or("").starts_with(prefix) {
                prefix.len()
            } else {
                0
            };
            self.doc.apply_block_edit(start..end, &out, start, start);
            let c = self.editor.cursor.saturating_sub(removed).max(start).min(self.doc.text.len());
            let sel = self.editor.sel.min(self.doc.text.len());
            let blk = self.doc.block_at(c);
            self.editor.set_caret(c, Some(blk));
            self.editor.sel = sel;
        } else {
            self.doc.apply_block_edit(start..end, &out, start, start);
            let c = (self.editor.cursor + prefix.len()).min(self.doc.text.len());
            let sel = (self.editor.sel + prefix.len()).min(self.doc.text.len());
            let blk = self.doc.block_at(c);
            self.editor.set_caret(c, Some(blk));
            self.editor.sel = sel;
        }
        self.editor.reading = false;
    }

    /// Turn the caret's block into a heading (`0` = back to a paragraph).
    pub fn set_heading(&mut self, level: u8) {
        let cursor = self.editor.cursor.min(self.doc.text.len());
        let idx = self.doc.block_at(cursor);
        let Some(block) = self.doc.parsed.blocks.get(idx) else {
            return;
        };
        let range = block.edit_range(&self.doc.text);
        let raw = self.doc.text.get(range.clone()).unwrap_or("").to_string();
        if raw.is_empty() && level == 0 {
            return;
        }

        // Drop an existing ATX marker and any setext underline.
        let mut lines: Vec<String> = raw.split('\n').map(|s| s.to_string()).collect();
        if let Some(first) = lines.first_mut() {
            let trimmed = first.trim_start();
            let hashes = trimmed.chars().take_while(|c| *c == '#').count();
            if hashes >= 1 && hashes <= 6 {
                let rest = trimmed[hashes..].trim_start();
                if rest.is_empty() || trimmed.as_bytes().get(hashes) == Some(&b' ') {
                    *first = rest.to_string();
                }
            }
        }
        if lines.len() > 1 {
            let last = lines.last().map(|s| s.trim().to_string()).unwrap_or_default();
            let setext = !last.is_empty()
                && (last.chars().all(|c| c == '=') || last.chars().all(|c| c == '-'))
                && last.len() >= 3;
            if setext {
                lines.pop();
            }
        }

        let body = lines.join("\n");
        let new = if level == 0 {
            body
        } else {
            format!("{} {}", "#".repeat(level.clamp(1, 6) as usize), body)
        };
        if new != raw {
            self.doc.apply_block_edit(range, &new, cursor, cursor);
        }

        // Put the caret just after the new marker.
        let first_len = new.split('\n').next().unwrap_or("").len();
        let c = (block_start(&self.doc, idx) + first_len).min(self.doc.text.len());
        let blk = self.doc.block_at(c);
        self.editor.set_caret(c, Some(blk));
        self.editor.reading = false;
    }

    /// Insert `text` as a new block after the caret's block.
    pub fn insert_block(&mut self, text: &str) {
        let cursor = self.editor.cursor.min(self.doc.text.len());
        let idx = self.doc.block_at(cursor);
        let Some(block) = self.doc.parsed.blocks.get(idx) else {
            return;
        };
        let at = block.range.end;
        let ins = format!("{text}\n\n");
        self.doc.insert_at(at, &ins, at);
        let c = at + ins.len();
        let blk = self.doc.block_at(c);
        self.editor.set_caret(c, Some(blk));
        self.editor.scroll_to_block = Some(blk);
        self.editor.reading = false;
    }

    pub fn toggle_reading(&mut self) {
        self.editor.reading = !self.editor.reading;
        if self.editor.reading {
            self.pane = Pane::Edit;
        }
    }

    pub fn jump_to_block(&mut self, block: usize) {
        let Some(b) = self.doc.parsed.blocks.get(block) else {
            return;
        };
        let off = b.range.start;
        self.editor.reading = false;
        self.editor.set_caret(off, Some(block));
        self.editor.scroll_to_block = Some(block);
    }

    // ------------------------------------------------------------------ tree

    pub fn refresh_tree(&mut self) {
        let Some(root) = self.tree_root.clone() else {
            self.tree.clear();
            return;
        };
        let mut budget = 4000usize;
        self.tree = read_dir_tree(&root, 0, &mut budget);
    }

    // ------------------------------------------------------------------ find

    pub fn recompute_matches(&mut self) {
        self.find.matches = find_all(
            &self.doc.text,
            &self.find.query,
            self.find.case_sensitive,
            self.find.whole_word,
        );
        if self.find.matches.is_empty() {
            self.find.current = 0;
        } else if self.find.current >= self.find.matches.len() {
            self.find.current = self.find.matches.len() - 1;
        }
    }

    fn goto_match(&mut self, index: usize) {
        let Some((start, end)) = self.find.matches.get(index).copied() else {
            return;
        };
        let blk = self.doc.block_at(start);
        self.editor.set_caret(start, Some(blk));
        self.editor.cursor = end;
        self.editor.scroll_to_block = Some(blk);
    }

    pub fn find_next(&mut self) {
        if self.find.matches.is_empty() {
            self.recompute_matches();
        }
        if self.find.matches.is_empty() {
            self.toast("未找到匹配");
            return;
        }
        self.find.current = (self.find.current + 1) % self.find.matches.len();
        let i = self.find.current;
        self.goto_match(i);
    }

    pub fn find_prev(&mut self) {
        if self.find.matches.is_empty() {
            self.recompute_matches();
        }
        if self.find.matches.is_empty() {
            self.toast("未找到匹配");
            return;
        }
        let n = self.find.matches.len();
        self.find.current = (self.find.current + n - 1) % n;
        let i = self.find.current;
        self.goto_match(i);
    }

    pub fn replace_current(&mut self) {
        let Some((start, end)) = self.find.matches.get(self.find.current).copied() else {
            self.toast("未找到匹配");
            return;
        };
        let replacement = self.find.replace.clone();
        self.doc
            .apply_block_edit(start..end, &replacement, start, start);
        let next = self.find.current.min(self.find.matches.len().saturating_sub(1));
        self.recompute_matches();
        self.find.current = next.min(self.find.matches.len().saturating_sub(1));
        if !self.find.matches.is_empty() {
            let i = self.find.current;
            self.goto_match(i);
        }
    }

    pub fn replace_all(&mut self) {
        if self.find.matches.is_empty() {
            self.recompute_matches();
        }
        if self.find.matches.is_empty() {
            self.toast("未找到匹配");
            return;
        }
        let mut out = String::with_capacity(self.doc.text.len());
        let mut last = 0usize;
        for (start, end) in &self.find.matches {
            out.push_str(&self.doc.text[last..*start]);
            out.push_str(&self.find.replace);
            last = *end;
        }
        out.push_str(&self.doc.text[last..]);
        let count = self.find.matches.len();
        let len = self.doc.text.len();
        self.doc.apply_block_edit(0..len, &out, 0, 0);
        self.find.matches.clear();
        self.find.current = 0;
        let blk = self.doc.block_at(0);
        self.editor.set_caret(0, Some(blk));
        self.toast(format!("已替换 {count} 处"));
    }

    // ---------------------------------------------------------------- prompts

    fn ask_unsaved(&mut self, pending: Pending) {
        let name = self.doc.display_name();
        self.pending = Some(pending);
        self.prompt = Some(
            Prompt::new(
                "未保存的更改",
                format!("「{name}」有未保存的修改。要先保存吗？"),
            )
            .choice("保存", PromptChoice::Save)
            .choice("不保存", PromptChoice::Discard)
            .choice("取消", PromptChoice::Cancel),
        );
    }

    pub fn resolve_prompt(&mut self, choice: PromptChoice) {
        let pending = self.pending.take();
        self.prompt = None;
        match choice {
            PromptChoice::Cancel => {}
            PromptChoice::Ok => {}
            PromptChoice::Save => {
                let saved = self.save();
                if saved {
                    self.finish_pending(pending);
                }
            }
            PromptChoice::Discard => self.finish_pending(pending),
        }
    }

    fn finish_pending(&mut self, pending: Option<Pending>) {
        match pending {
            None => {}
            Some(Pending::Quit) => {
                self.quit_ok = true;
                self.cfg.save();
            }
            Some(Pending::NewDoc) => {
                self.doc.dirty = false;
                self.reset_document();
            }
            Some(Pending::OpenPath(p)) => {
                self.doc.dirty = false;
                self.load_path(&p);
            }
        }
    }

    pub fn request_quit(&mut self) {
        if self.doc.dirty {
            self.ask_unsaved(Pending::Quit);
        } else {
            self.quit_ok = true;
            self.cfg.save();
        }
    }

    /// Called after a successful save so a queued action can run.
    fn after_save(&mut self) {
        if let Some(p) = self.pending.take() {
            self.prompt = None;
            self.finish_pending(Some(p));
        }
    }

    // ----------------------------------------------------------------- toast

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    // ------------------------------------------------------------------ draw

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.prompt.is_some() {
            return;
        }
        if consume_cmd(ctx, Key::S, true) {
            self.save_as_dialog();
            return;
        }
        if consume_cmd(ctx, Key::S, false) {
            self.save();
            return;
        }
        if consume_cmd(ctx, Key::O, true) {
            self.open_system_dialog();
            return;
        }
        if consume_cmd(ctx, Key::O, false) {
            self.open_dialog();
            return;
        }
        if consume_cmd(ctx, Key::N, false) {
            self.new_document();
            return;
        }
        if consume_cmd(ctx, Key::F, false) {
            self.find.open = true;
            self.find.focus_query = true;
            return;
        }
        if consume_cmd(ctx, Key::R, false) {
            self.toggle_reading();
            return;
        }
        if consume_cmd(ctx, Key::Slash, false) {
            self.cfg.theme = self.cfg.theme.toggled();
            self.cfg.save();
            return;
        }
        if consume_cmd(ctx, Key::Z, true) {
            self.redo();
            return;
        }
        if consume_cmd(ctx, Key::Z, false) {
            self.undo();
            return;
        }
        if consume_cmd(ctx, Key::B, false) {
            self.wrap_selection("**", "**");
            return;
        }
        if consume_cmd(ctx, Key::I, false) {
            self.wrap_selection("*", "*");
            return;
        }
        if consume_cmd(ctx, Key::E, true) {
            self.toggle_pane();
            return;
        }
        if consume_cmd(ctx, Key::E, false) {
            self.wrap_selection("`", "`");
            return;
        }
        if consume_cmd(ctx, Key::K, false) {
            self.wrap_selection("[", "](https://)");
            return;
        }
        // In the 源码 pane the caret lives in a plain text widget, so leave
        // ⌘A to it rather than selecting a single block.
        if self.pane == Pane::Edit && consume_cmd(ctx, Key::A, false) {
            self.select_all();
            return;
        }
        for level in 0..=6u8 {
            let key = match level {
                0 => Key::Num0,
                1 => Key::Num1,
                2 => Key::Num2,
                3 => Key::Num3,
                4 => Key::Num4,
                5 => Key::Num5,
                _ => Key::Num6,
            };
            if consume_cmd(ctx, key, false) {
                self.set_heading(level);
                return;
            }
        }
        if consume_cmd(ctx, Key::Escape, false) {
            self.find.open = false;
        }
    }

    fn central(&mut self, ui: &mut Ui, ec: &EditorCtx) {
        match self.pane {
            Pane::Edit => {
                self.editor.show(ui, &mut self.doc, ec);
            }
            Pane::Source => {
                self.source_editor(ui, ec);
            }
            Pane::Split => {
                let full = ui.available_width();
                let half = (full * 0.5 - 10.0).max(120.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(half, ui.available_height()),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            self.editor.show(ui, &mut self.doc, ec);
                        },
                    );
                    ui.separator();
                    ui.allocate_ui_with_layout(
                        Vec2::new(half, ui.available_height()),
                        egui::Layout::top_down(Align::Min),
                        |ui| {
                            let ec2 = EditorCtx {
                                content_width: half,
                                ..ec.clone()
                            };
                            self.source_editor(ui, &ec2);
                        },
                    );
                });
            }
        }
    }

    /// A whole-document Markdown editor. Edits go through [`Document`] so the
    /// same undo stack, dirty flag and re-parse apply as in the live editor.
    fn source_editor(&mut self, ui: &mut Ui, ec: &EditorCtx) {
        let side = ((ui.available_width() - ec.content_width) * 0.5).max(12.0);
        let mut buf = self.doc.text.clone();
        let mut changed = false;
        ScrollArea::vertical()
            .id_salt("rustmd-source")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_max_width(ec.content_width);
                        let family = fonts::mono_family_for(false);
                        let resp = ui.add(
                            TextEdit::multiline(&mut buf)
                                .id(egui::Id::new("rustmd-source-edit"))
                                .frame(false)
                                .desired_width(f32::INFINITY)
                                .desired_rows(30)
                                .font(egui::FontId::new(ec.base * 0.94, family))
                                .lock_focus(true),
                        );
                        if resp.changed() {
                            changed = true;
                        }
                    });
                });
            });
        if changed {
            let len = self.doc.text.len();
            let cursor = self.editor.cursor.min(self.doc.text.len());
            self.doc.apply_block_edit(0..len, &buf, cursor, cursor);
            let c = self.editor.cursor.min(self.doc.text.len());
            let blk = self.doc.block_at(c);
            self.editor.active = Some(blk);
            self.editor.cursor = c;
            self.editor.sel = self.editor.sel.min(self.doc.text.len());
        }
    }

    fn autosave(&mut self) {
        let secs = self.cfg.autosave_secs;
        if secs == 0 || !self.doc.dirty || self.doc.path.is_none() {
            return;
        }
        if self.last_autosave.elapsed().as_secs() >= secs {
            self.last_autosave = Instant::now();
            let _ = self.doc.save();
        }
    }
}

// ===========================================================================
// Window layout
// ===========================================================================

impl App {
    /// One frame of the whole window: menu bar, status bar, both side panels,
    /// the document, and every overlay.
    ///
    /// Split out of the `eframe::App` impl because it needs nothing from
    /// `eframe` — and because a test can then drive a real frame and check that
    /// the window is actually covered by the shapes that get painted.
    pub fn ui(&mut self, ctx: &egui::Context) {
        self.ctx = Some(ctx.clone());

        // The window's close button goes through the same unsaved-changes gate.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_ok {
            if self.doc.dirty {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.request_quit();
            } else {
                self.quit_ok = true;
            }
        }

        if self.cfg.theme != self.applied_theme {
            self.theme = Theme::for_mode(self.cfg.theme);
            self.theme.apply(ctx);
            self.applied_theme = self.cfg.theme;
            self.cfg.save();
        }

        self.handle_finder_open();
        self.handle_drops(ctx);
        self.shortcuts(ctx);

        let ec = EditorCtx {
            theme: self.theme.clone(),
            base: self.cfg.font_size,
            line_height: self.cfg.line_height,
            content_width: self.cfg.content_width,
            wrap_code: self.cfg.wrap_code,
            focus_mode: self.cfg.focus_mode,
            typewriter: self.cfg.typewriter,
            doc_dir: self.doc.dir(),
        };

        let chrome = egui::Frame::NONE
            .fill(self.theme.chrome)
            .inner_margin(egui::Margin::symmetric(8, 4));
        let bar = egui::Frame::NONE
            .fill(self.theme.chrome)
            .inner_margin(egui::Margin::symmetric(10, 3));
        let side_frame = egui::Frame::NONE
            .fill(self.theme.sidebar)
            .inner_margin(egui::Margin::symmetric(10, 8));
        let page_frame = egui::Frame::NONE
            .fill(self.theme.bg)
            .inner_margin(egui::Margin::symmetric(0, 10));
        let text_color = self.theme.text;

        egui::TopBottomPanel::top("rustmd-menu")
            .frame(chrome)
            .show(ctx, |ui| panels::menu_bar(ui, self));

        egui::TopBottomPanel::bottom("rustmd-status")
            .frame(bar)
            .show(ctx, |ui| panels::status_bar(ui, self));

        if self.find.open {
            egui::TopBottomPanel::bottom("rustmd-find")
                .frame(bar)
                .show(ctx, |ui| panels::find_bar(ui, self));
        }

        if self.cfg.show_sidebar {
            let r = egui::SidePanel::left("rustmd-files")
                .resizable(true)
                .default_width(self.cfg.sidebar_width)
                .width_range(160.0..=460.0)
                .frame(side_frame)
                .show(ctx, |ui| panels::file_tree(ui, self));
            let w = r.response.rect.width();
            if w > 1.0 {
                self.cfg.sidebar_width = w;
            }
        }

        if self.cfg.show_outline {
            let r = egui::SidePanel::right("rustmd-outline")
                .resizable(true)
                .default_width(self.cfg.outline_width)
                .width_range(150.0..=420.0)
                .frame(side_frame)
                .show(ctx, |ui| panels::outline(ui, self));
            let w = r.response.rect.width();
            if w > 1.0 {
                self.cfg.outline_width = w;
            }
        }

        let focus = self.cfg.focus_mode;
        egui::CentralPanel::default()
            .frame(page_frame)
            .show(ctx, |ui| {
                if focus {
                    ui.visuals_mut().override_text_color = Some(text_color);
                }
                self.central(ui, &ec);
            });

        panels::dialogs(ctx, self);
        panels::drop_overlay(ctx, self);

        // Remember the window geometry so the next launch lands in the same spot.
        if let Some(r) = ctx.input(|i| i.viewport().inner_rect) {
            if r.width() > 200.0 && r.height() > 150.0 {
                self.cfg.window = Some((r.min.x, r.min.y, r.width(), r.height()));
            }
        }

        if let Some((_, at)) = &self.toast {
            if at.elapsed().as_secs_f32() > TOAST {
                self.toast = None;
            }
        }

        self.autosave();

        if self.quit_ok {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// The `eframe` entry points. Both are one-liners — everything they need lives
/// on `App` itself, which is what keeps the window layout testable.
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui(ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        // Whatever is not covered by a shape shows this. The default is
        // transparent, which macOS composites to black — so a panel that
        // forgets to paint its own background shows up as a black bar rather
        // than as something merely wrong-looking. Match the theme instead.
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.cfg.save();
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Give `path` the extension `ext` if it has none.
///
/// The OS save panels are called without a content filter, so they will happily
/// return a bare file name; this puts the extension back.
fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    if path.extension().is_some() {
        path
    } else {
        path.with_extension(ext)
    }
}

/// Consume a ⌘-shortcut, telling the shifted and unshifted forms apart.
///
/// `InputState::consume_key` matches *logically*, and a pattern without `shift`
/// happily matches a press that has it. Routing every ⌘-shortcut through this
/// helper is what keeps ⇧⌘Z as redo instead of undo, and ⇧⌘S as "save as"
/// instead of "save".
fn consume_cmd(ctx: &egui::Context, key: Key, shift: bool) -> bool {
    if ctx.input(|i| i.modifiers.shift) != shift {
        return false;
    }
    let pattern = if shift {
        Modifiers::COMMAND | Modifiers::SHIFT
    } else {
        Modifiers::COMMAND
    };
    ctx.input_mut(|i| i.consume_key(pattern, key))
}

fn block_start(doc: &Document, idx: usize) -> usize {
    doc.parsed
        .blocks
        .get(idx)
        .map(|b| b.range.start)
        .unwrap_or(0)
}

/// Read a directory into a tree, skipping the usual noise.
fn read_dir_tree(dir: &Path, depth: usize, budget: &mut usize) -> Vec<TreeNode> {
    if depth > 6 || *budget == 0 {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.flatten() {
        if *budget == 0 {
            break;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            if matches!(name.as_str(), "target" | "node_modules" | "dist" | "build") {
                continue;
            }
            *budget -= 1;
            let children = read_dir_tree(&path, depth + 1, budget);
            dirs.push(TreeNode {
                name,
                path,
                is_dir: true,
                children,
            });
        } else if ft.is_file() {
            // One definition of "a file this program can open", shared with the
            // built-in browser, so the two listings can never disagree.
            if !picker::is_textish(&path) {
                continue;
            }
            *budget -= 1;
            files.push(TreeNode {
                name,
                path,
                is_dir: false,
                children: Vec::new(),
            });
        }
    }
    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    dirs.extend(files);
    dirs
}

/// Find every occurrence of `query`, honouring case and whole-word settings.
fn find_all(text: &str, query: &str, case_sensitive: bool, whole_word: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    let hay: Vec<char> = text.chars().collect();
    let needle: Vec<char> = query.chars().collect();
    let (n, m) = (hay.len(), needle.len());
    if m == 0 || m > n {
        return out;
    }
    // Byte offset of every character, plus one past the end.
    let mut offs = Vec::with_capacity(n + 1);
    let mut b = 0usize;
    for c in &hay {
        offs.push(b);
        b += c.len_utf8();
    }
    offs.push(b);

    let eq = |a: char, b: char| {
        if case_sensitive {
            a == b
        } else {
            a.to_lowercase().eq(b.to_lowercase())
        }
    };
    let boundary = |c: Option<char>| match c {
        None => true,
        Some(c) => !c.is_alphanumeric() && c != '_',
    };

    let mut i = 0usize;
    while i + m <= n {
        if (0..m).all(|k| eq(hay[i + k], needle[k])) {
            let ok = !whole_word
                || (boundary(if i == 0 { None } else { Some(hay[i - 1]) })
                    && boundary(hay.get(i + m).copied()));
            if ok {
                out.push((offs[i], offs[i + m]));
            }
            i += m;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_is_case_insensitive_by_default() {
        let m = find_all("Hello hello HELLO", "hello", false, false);
        assert_eq!(m.len(), 3);
        assert_eq!(&"Hello hello HELLO"[m[0].0..m[0].1], "Hello");
    }

    #[test]
    fn find_respects_case_and_words() {
        let m = find_all("cat category cat", "cat", true, true);
        assert_eq!(m.len(), 2);
        let m = find_all("cat category Cat", "cat", false, true);
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn find_handles_multibyte() {
        let text = "中文测试中文";
        let m = find_all(text, "中文", false, false);
        assert_eq!(m.len(), 2);
        for (a, b) in m {
            assert_eq!(&text[a..b], "中文");
        }
    }
}
