//! The live-preview editor widget.
//!
//! This is the Typora model implemented on top of `egui`:
//!
//! * Exactly one block is *active*. It is drawn as a real multiline text editor
//!   holding that block's raw Markdown, with the structural markers dimmed and
//!   inline styling (bold, code, links, formulas) applied live by the layouter.
//! * Every other block is drawn fully rendered by [`crate::render`].
//! * Clicking a rendered block activates it and places the caret at the clicked
//!   character, using the byte-range map recorded while rendering.
//! * Structural keys (Backspace at a boundary, Tab, the arrow keys at the first
//!   and last visual line) are intercepted *before* the text widget sees them,
//!   so the document is edited as a whole rather than one block at a time.
//!
//! Because a block always knows the exact byte range it occupies in the
//! original text, every edit round-trips: the document stays a plain string and
//! the block list is derived from it.

use std::path::PathBuf;
use std::sync::Arc;

use egui::text::{CCursor, CCursorRange, LayoutJob, LayoutSection};
use egui::text_edit::TextEditState;
use egui::{
    Align, Color32, CursorIcon, FontId, Id, Key, Modifiers, Pos2, Rect, ScrollArea, Sense, TextEdit,
    Ui, vec2,
};

use crate::code_hl;
use crate::doc::Document;
use crate::fonts;
use crate::parser::{self, BlockKind, Leaf};
use crate::render::{self, Hit, RenderCtx};
use crate::theme::Theme;

const ACTIVE_ID: &str = "rustmd-active-block";

#[derive(Clone)]
pub struct EditorCtx {
    /// A copy of the palette: owning it keeps the editor free of borrows into
    /// the application while it mutates the document.
    pub theme: Theme,
    pub base: f32,
    pub line_height: f32,
    pub content_width: f32,
    pub wrap_code: bool,
    pub focus_mode: bool,
    pub typewriter: bool,
    pub doc_dir: Option<PathBuf>,
}

pub struct Editor {
    /// Caret position: a byte offset into the document.
    pub cursor: usize,
    /// Selection anchor: a byte offset into the document.
    pub sel: usize,
    /// The block currently rendered as raw, editable Markdown.
    pub active: Option<usize>,
    /// Pure reading: nothing is editable and every block is rendered.
    pub reading: bool,
    /// Ask the scroll area to reveal this block on the next frame.
    pub scroll_to_block: Option<usize>,
    /// How far down the document is scrolled, in `0.0..=1.0`.
    pub scroll_percent: f32,

    /// Restore the horizontal caret position after a vertical move.
    goal: Option<(f32, bool)>,
    push_cursor: bool,
    row: usize,
    rows: usize,
    /// Where every block sits, so a frame only lays out what is on screen.
    heights: Heights,
    /// Block indices the current frame draws, kept between frames so the
    /// common case does not allocate.
    plan: Vec<usize>,

    /// Height the scroll area laid the document out at last frame. It has to
    /// come out exactly as tall as the cache says the document is; a
    /// difference is empty space that scrolls into view.
    #[cfg(test)]
    laid_out: f32,
    /// How far the document was scrolled last frame.
    #[cfg(test)]
    offset: f32,
    /// Where the last frame put each block it drew: `(block, layout top)`.
    #[cfg(test)]
    drawn: Vec<(usize, f32)>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            cursor: 0,
            sel: 0,
            active: None,
            reading: false,
            scroll_to_block: None,
            scroll_percent: 0.0,
            goal: None,
            push_cursor: false,
            row: 0,
            rows: usize::MAX,
            heights: Heights::default(),
            plan: Vec::new(),
            #[cfg(test)]
            laid_out: 0.0,
            #[cfg(test)]
            offset: 0.0,
            #[cfg(test)]
            drawn: Vec::new(),
        }
    }
}

impl Editor {
    /// How many blocks the last frame laid out.
    ///
    /// With the height cache warm this is the size of the window on screen, not
    /// the size of the document — which is the whole point of the cache, and
    /// what the benchmark reports.
    pub fn drawn_blocks(&self) -> usize {
        self.plan.len()
    }

    /// Height the scroll area laid the document out at, last frame.
    #[cfg(test)]
    pub fn laid_out_height(&self) -> f32 {
        self.laid_out
    }

    /// Height of the whole document according to the height cache.
    #[cfg(test)]
    pub fn document_height(&self) -> f32 {
        self.heights.total()
    }

    /// How far down the document was scrolled last frame.
    #[cfg(test)]
    pub fn scroll_offset(&self) -> f32 {
        self.offset
    }

    /// Distance from the top of the document to the top of block `i`.
    #[cfg(test)]
    pub fn block_top(&self, i: usize) -> f32 {
        self.heights.top(i)
    }

    /// `(block, layout top)` for every block the last frame drew.
    #[cfg(test)]
    pub fn drawn_layout_tops(&self) -> &[(usize, f32)] {
        &self.drawn
    }

    pub fn set_caret(&mut self, offset: usize, block: Option<usize>) {        self.cursor = offset;
        self.sel = offset;
        self.active = block;
        self.reading = false;
        self.push_cursor = true;
        self.rows = usize::MAX;
        self.row = 0;
    }

    pub fn select_all(&mut self, doc: &Document) {
        let idx = self
            .active
            .unwrap_or_else(|| doc.block_at(self.cursor.min(doc.text.len())));
        if let Some(b) = doc.parsed.blocks.get(idx) {
            let r = b.edit_range(&doc.text);
            self.active = Some(idx);
            self.sel = r.start;
            self.cursor = r.end;
            self.push_cursor = true;
        }
    }

    pub fn show(&mut self, ui: &mut Ui, doc: &mut Document, ec: &EditorCtx) {
        if doc.parsed.blocks.is_empty() {
            doc.reparse();
        }

        let avail = ui.available_width();
        let side = ((avail - ec.content_width) * 0.5).max(14.0);
        let col_w = (avail - side * 2.0).max(120.0);

        if self.reading {
            self.active = None;
        } else if self.active.map_or(true, |a| a >= doc.parsed.blocks.len()) {
            let b = doc.block_at(self.cursor.min(doc.text.len()));
            self.active = Some(b);
            self.push_cursor = true;
        }

        let id = Id::new(ACTIVE_ID);
        let focused = ui.memory(|m| m.has_focus(id));
        self.pre_keys(ui, doc, focused);

        let rctx = RenderCtx {
            parsed: doc.parsed.clone(),
            theme: &ec.theme,
            base: ec.base,
            line_height: ec.line_height,
            content_width: col_w,
            wrap_code: ec.wrap_code,
            doc_dir: ec.doc_dir.clone(),
            active: self.active,
            focus_mode: ec.focus_mode,
        };

        let active = self.active;
        let mut pending: Option<(usize, Pos2, Vec<Hit>, Rect)> = None;
        let mut clicked_anywhere = false;
        let mut toggled: Option<usize> = None;
        let mut content_bottom = 0.0f32;
        let mut content_top = 0.0f32;
        let mut first_rect = true;
        let mut reached_end = false;
        let mut scroll_offset: Option<f32> = None;

        // Laying out the whole document is what makes a large file slow, so
        // only the blocks on screen are drawn. Where they go comes from the
        // height cache, which is filled by laying the document out once and
        // then kept up to date by the blocks that are actually drawn.
        let spacing = ui.spacing().item_spacing.y;
        let viewport_h = ui.available_height();
        let mut heights = std::mem::take(&mut self.heights);
        let mut plan = std::mem::take(&mut self.plan);
        let n = doc.parsed.blocks.len();
        heights.prepare(&doc.parsed, col_w, ec.base, ec.line_height, spacing);
        let measuring = !heights.complete();
        // Until the heights are known a frame has to draw everything, and then
        // the block to reveal is on screen already and can be scrolled to
        // directly. Afterwards it is cheaper to place the viewport by
        // arithmetic, which also works for a block that is far away.
        let scroll_target = if measuring {
            self.scroll_to_block.take()
        } else {
            let offset = self
                .scroll_to_block
                .take()
                .and_then(|i| heights.offset_for(i, viewport_h));
            if let Some(offset) = offset {
                scroll_offset = Some(offset);
            }
            None
        };

        let sa = {
            let mut area = ScrollArea::vertical()
                .id_salt("rustmd-doc")
                .auto_shrink([false, false]);
            if let Some(offset) = scroll_offset {
                area = area.vertical_scroll_offset(offset);
            }
            area.show_viewport(ui, |ui, viewport| {
                #[cfg(test)]
                self.drawn.clear();
                ui.horizontal_top(|ui| {
                    ui.add_space(side);
                    ui.vertical(|ui| {
                        ui.set_max_width(col_w);
                        plan.clear();
                        if measuring {
                            plan.extend(0..n);
                        } else {
                            heights.window(viewport, n, &mut plan);
                        }
                        // The block being edited is drawn even when it is off
                        // screen: it owns the keyboard focus and the caret, and
                        // dropping it for a frame would throw both away.
                        if let Some(a) = active {
                            if a < n && plan.binary_search(&a).is_err() {
                                let at = plan.partition_point(|&x| x < a);
                                plan.insert(at, a);
                            }
                        }
                        reached_end = plan.last() == Some(&n.saturating_sub(1));

                        // The running position is kept in *document*
                        // coordinates throughout, the same space `heights.top`
                        // is in. The layout cursor is not: it is measured from
                        // the top of the window, which sits below the scroll
                        // area's own origin by exactly the scroll offset. Using
                        // the cursor itself would therefore reintroduce the
                        // offset as blank space above every block below the
                        // first one drawn — the document would stop moving and
                        // a band as tall as the scroll position would open up.
                        let mut flow = 0.0f32;
                        for &i in &plan {
                            let want = heights.top(i);
                            if want > flow {
                                ui.add_space(want - flow);
                            }
                            let before = ui.cursor().top();
                            #[cfg(test)]
                            self.drawn.push((i, before));
                            if Some(i) == active {
                                let rect = self.edit_block(ui, doc, i, &rctx, ec);
                                if first_rect {
                                    content_top = rect.top();
                                    first_rect = false;
                                }
                                content_bottom = content_bottom.max(rect.bottom());
                                if scroll_target == Some(i) {
                                    ui.scroll_to_rect(
                                        rect.expand2(vec2(0.0, 140.0)),
                                        Some(Align::Center),
                                    );
                                }
                            } else {
                                let r = render::render_block(ui, &rctx, i);
                                let rect = r.rect;
                                let toggle = r.toggled_task;
                                if first_rect {
                                    content_top = rect.top();
                                    first_rect = false;
                                }
                                content_bottom = content_bottom.max(rect.bottom());
                                if let Some(t) = toggle {
                                    toggled = Some(t);
                                }
                                if scroll_target == Some(i) {
                                    ui.scroll_to_rect(
                                        rect.expand2(vec2(0.0, 140.0)),
                                        Some(Align::Center),
                                    );
                                }
                                if rect.width() > 1.0 {
                                    let resp = ui
                                        .interact(rect, Id::new(("rustmd-blk", i)), Sense::click())
                                        .on_hover_cursor(CursorIcon::Text);
                                    if resp.clicked() {
                                        clicked_anywhere = true;
                                        if let Some(pos) = resp.interact_pointer_pos() {
                                            pending = Some((i, pos, r.hits, rect));
                                        }
                                    }
                                }
                            }
                            // How far the layout cursor actually moved is the
                            // only measurement that survives every kind of
                            // block, and the trailing spacing is not part of
                            // the block.
                            let after = ui.cursor().top();
                            let height = (after - before - spacing).max(0.0);
                            heights.record(i, height);
                            flow = want + height + spacing;
                        }
                        // Everything below the last drawn block still has to
                        // take up room, or the scrollbar would shrink to fit
                        // what happens to be on screen. On the frame that drew
                        // the whole document there is nothing below it, and
                        // the running total is already the real one.
                        if !measuring {
                            let tail = heights.total() - flow;
                            if tail > 0.0 {
                                ui.add_space(tail);
                            }
                        }
                        ui.add_space(ui.available_height().max(0.0));
                    });
                });
            })
        };

        if measuring {
            heights.mark_measured(n);
        }
        self.heights = heights;
        self.plan = plan;

        let span = (sa.content_size.y - sa.inner_rect.height()).max(0.0);
        #[cfg(test)]
        {
            self.laid_out = sa.content_size.y;
            self.offset = sa.state.offset.y;
        }
        self.scroll_percent = if span <= 1.0 {
            0.0
        } else {
            (sa.state.offset.y / span).clamp(0.0, 1.0)
        };

        if let Some(idx) = toggled {
            self.toggle_task(doc, idx);
        }
        if let Some((idx, pos, hits, rect)) = pending {
            self.activate_from_click(ui, doc, ec, idx, pos, &hits, rect);
        } else if !clicked_anywhere {
            // A click on empty space below the document puts the caret at the
            // end — but only when the end is actually the last thing drawn.
            // Scrolled to the middle, the space under the last drawn block is
            // not the end of the document.
            let clicked = ui.input(|i| i.pointer.any_click());
            if clicked && reached_end {
                if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                    if pos.y > content_bottom && pos.y > content_top {
                        let end = doc.text.len();
                        let b = doc.block_at(end);
                        self.set_caret(end, Some(b));
                    }
                }
            }
        }

    }

    // =======================================================================
    // Interaction
    // =======================================================================

    fn toggle_task(&mut self, doc: &mut Document, idx: usize) {
        let Some(block) = doc.parsed.blocks.get(idx) else {
            return;
        };
        let BlockKind::ListItem(li) = &block.kind else {
            return;
        };
        let Some(done) = li.task else { return };
        let start = block.range.start;
        let line_end = doc.line_end(start);
        let line = doc.text[start..line_end].to_string();
        let Some(open) = line.find('[') else { return };
        if open + 2 >= line.len() {
            return;
        }
        let new = if done { " " } else { "x" };
        let at = start + open + 1;
        doc.apply_block_edit(at..at + 1, new, self.cursor, self.sel);
    }

    fn activate_from_click(
        &mut self,
        ui: &Ui,
        doc: &mut Document,
        ec: &EditorCtx,
        idx: usize,
        pos: Pos2,
        hits: &[Hit],
        block_rect: Rect,
    ) {
        let Some(block) = doc.parsed.blocks.get(idx) else {
            return;
        };
        let off_in_content = nearest_content_offset(hits, pos, block);
        let src = block.src_of(off_in_content);
        let src = if hits.is_empty() {
            line_offset_from_y(doc, ec, idx, pos, block_rect)
        } else {
            src
        };

        let modified = ui.input(|i| i.modifiers.command || i.modifiers.ctrl);
        if modified {
            if let Some(url) = link_at(doc, idx, off_in_content) {
                open_url(&url);
                return;
            }
        }

        self.reading = false;
        self.active = Some(idx);
        self.cursor = src.min(doc.text.len());
        self.sel = self.cursor;
        self.push_cursor = true;
        self.rows = usize::MAX;
        self.row = 0;
    }

    // =======================================================================
    // Keys
    // =======================================================================

    fn pre_keys(&mut self, ui: &mut Ui, doc: &mut Document, focused: bool) {
        if !focused {
            return;
        }
        let Some(idx) = self.active else { return };
        if idx >= doc.parsed.blocks.len() {
            return;
        }
        let goal = self.goal;

        // The arrow keys only leave the block from its first and last visual
        // line. Everywhere else they belong to the text widget, so the event is
        // left untouched — consuming it here would freeze the caret inside
        // multi-line and wrapped blocks.
        if self.row == 0 && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowUp)) {
            self.move_block(doc, idx, -1, goal);
            return;
        }
        if self.row + 1 >= self.rows
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::ArrowDown))
        {
            self.move_block(doc, idx, 1, goal);
            return;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.reading = true;
            self.active = None;
            return;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Enter)) {
            let at = self.cursor.min(doc.text.len());
            doc.insert_at(at, "  \n", at);
            let n = at + 3;
            self.set_caret(n, Some(doc.block_at(n)));
            return;
        }
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Backspace)) {
            self.handle_backspace(doc, idx);
            return;
        }
        // Shift+Tab has to be tested first: a pattern without `shift` also
        // matches a press that has it, so the plain branch would swallow it.
        if self.shift_held(ui)
            && ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Tab))
        {
            self.indent(doc, idx, false);
            return;
        }
        if !self.shift_held(ui) && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Tab)) {
            self.indent(doc, idx, true);
        }
    }

    /// Whether shift is down right now, so the shifted and unshifted forms of a
    /// key can be told apart before either is consumed.
    fn shift_held(&self, ui: &Ui) -> bool {
        ui.input(|i| i.modifiers.shift)
    }

    fn handle_backspace(&mut self, doc: &mut Document, idx: usize) {
        let (a, b) = (self.sel.min(self.cursor), self.sel.max(self.cursor));
        if a != b {
            doc.delete_range(a..b, a);
            self.set_caret(a, Some(doc.block_at(a)));
            return;
        }
        let Some(block) = doc.parsed.blocks.get(idx) else {
            return;
        };
        let range = block.edit_range(&doc.text);
        let at = self.cursor;

        if at <= range.start {
            if range.start == 0 {
                return;
            }
            let cut = range.start - 1;
            doc.delete_range(cut..range.start, cut);
            self.set_caret(cut, Some(doc.block_at(cut)));
            return;
        }
        if doc.marker_only_prefix(at) {
            let from = range.start;
            doc.delete_range(from..at, from);
            self.set_caret(from, Some(doc.block_at(from)));
            return;
        }
        let prev = prev_boundary(&doc.text, at);
        doc.delete_range(prev..at, prev);
        self.set_caret(prev, Some(doc.block_at(prev)));
    }

    fn indent(&mut self, doc: &mut Document, idx: usize, deeper: bool) {
        let Some(block) = doc.parsed.blocks.get(idx) else {
            return;
        };
        let range = block.edit_range(&doc.text);
        if deeper {
            let first = range.start;
            let mut ins = String::from("  ");
            // Indent continuation lines too, so a multi-line item stays one item.
            let mut p = range.start;
            while p < range.end {
                let le = doc.line_end(p).min(range.end);
                p = le + 1;
                if p < range.end {
                    ins.push_str("  ");
                }
            }
            let before = doc.text[first..range.end].to_string();
            let mut indented = String::new();
            for (k, l) in before.split('\n').enumerate() {
                if k > 0 {
                    indented.push('\n');
                }
                if !l.is_empty() {
                    indented.push_str("  ");
                }
                indented.push_str(l);
            }
            let _ = ins;
            doc.apply_block_edit(range.clone(), &indented, self.cursor, self.sel);
            let d = 2 * before.split('\n').filter(|l| !l.is_empty()).count().min(1);
            self.set_caret(self.cursor + d, Some(idx));
        } else {
            let start = range.start;
            let mut removed = 0;
            let mut p = start;
            while removed < 2 && p < doc.text.len() && doc.text.as_bytes()[p] == b' ' {
                p += 1;
                removed += 1;
            }
            if removed > 0 {
                doc.delete_range(start..p, start);
                let c = self.cursor.saturating_sub(removed);
                self.set_caret(c, Some(doc.block_at(c)));
            }
        }
    }

    fn move_block(&mut self, doc: &Document, idx: usize, delta: isize, goal: Option<(f32, bool)>) {
        let n = doc.parsed.blocks.len() as isize;
        let j = idx as isize + delta;
        self.rows = usize::MAX;
        if j < 0 || j >= n {
            return;
        }
        let nb = &doc.parsed.blocks[j as usize];
        let r = nb.edit_range(&doc.text);
        let target = if delta > 0 { r.start } else { r.end };
        self.active = Some(j as usize);
        self.cursor = target;
        self.sel = target;
        self.push_cursor = true;
        self.goal = goal.map(|(x, _)| (x, delta > 0));
        self.row = 0;
        self.reading = false;
    }

    // =======================================================================
    // The active block
    // =======================================================================

    fn edit_block(
        &mut self,
        ui: &mut Ui,
        doc: &mut Document,
        idx: usize,
        rctx: &RenderCtx<'_>,
        ec: &EditorCtx,
    ) -> Rect {
        let block = &doc.parsed.blocks[idx];
        let kind = block.kind.clone();
        let range = block.edit_range(&doc.text);
        let parsed = doc.parsed.clone();
        let mut buf = doc.text.get(range.clone()).unwrap_or("").to_string();

        let is_item = matches!(kind, BlockKind::ListItem(_));
        let lead = rctx_indent(block.quote, &kind);
        let gutter = if is_item { 24.0 } else { 0.0 };
        let (size, bold, lh) = match &kind {
            BlockKind::Heading { level } => (rctx.heading_size(*level), true, 1.34),
            BlockKind::Code(_) => (ec.base * 0.88, false, 1.62),
            _ => (ec.base, false, ec.line_height),
        };

        let cursor_before = self.cursor;
        let sel_before = self.sel;
        let id = Id::new(ACTIVE_ID);
        if self.push_cursor {
            push_caret(
                ui.ctx(),
                id,
                &buf,
                self.sel.saturating_sub(range.start),
                self.cursor.saturating_sub(range.start),
            );
            self.push_cursor = false;
        }

        let width = (rctx.content_width - lead - gutter).max(80.0);
        let mut geometry: Option<Geometry> = None;

        ui.horizontal_top(|ui| {
            if lead > 0.0 {
                ui.add_space(lead);
            }
            ui.vertical(|ui| {
                ui.set_max_width(width);
                if is_item {
                    ui.horizontal_top(|ui| {
                        ui.add_space(gutter);
                        geometry =
                            Some(run_text_edit(ui, &mut buf, id, size, bold, lh, &kind, &parsed, &ec.theme));
                    });
                } else {
                    geometry =
                        Some(run_text_edit(ui, &mut buf, id, size, bold, lh, &kind, &parsed, &ec.theme));
                }
            });
        });

        let geometry = geometry.unwrap_or(Geometry {
            rect: Rect::NOTHING,
            galley_pos: Pos2::ZERO,
            caret: None,
            galley: ui.fonts(|f| f.layout_no_wrap(String::new(), FontId::proportional(12.0), Color32::BLACK)),
        });
        let rect = geometry.rect;
        let caret_rect = geometry.caret;

        if let Some(state) = TextEditState::load(ui.ctx(), id) {
            if let Some(cr) = state.cursor.char_range() {
                let p = char_index_to_byte(&buf, cr.primary.index);
                let s = char_index_to_byte(&buf, cr.secondary.index);
                self.cursor = range.start + p;
                self.sel = range.start + s;
            }
        }

        if buf.as_str() != doc.text.get(range.clone()).unwrap_or("") {
            doc.apply_block_edit(range.clone(), &buf, cursor_before, sel_before);
            self.reading = false;
        }
        self.cursor = self.cursor.min(doc.text.len());
        self.sel = self.sel.min(doc.text.len());

        // Typing Enter or Backspace can move the caret into a neighbouring block.
        let still_inside = doc
            .parsed
            .blocks
            .get(idx)
            .map(|b| self.cursor >= b.range.start && self.cursor <= b.range.end)
            .unwrap_or(false);
        if !still_inside {
            self.active = Some(doc.block_at(self.cursor));
            self.rows = usize::MAX;
        }

        // Resolve the goal column now that a galley exists.
        if let Some((gx, from_end)) = self.goal.take() {
            let galley = geometry.galley.clone();
            let rows = galley.rows.len();
            let ri = if from_end { rows.saturating_sub(1) } else { 0 };
            if let Some(row) = galley.rows.get(ri) {
                let y = row.rect.center().y;
                let cur = galley.cursor_from_pos(egui::vec2(gx - geometry.galley_pos.x, y));
                push_caret_at(ui.ctx(), id, cur.ccursor.index);
                let byte = char_index_to_byte(&buf, cur.ccursor.index);
                self.cursor = range.start + byte;
                self.sel = self.cursor;
            }
        }

        if ec.typewriter && ui.memory(|m| m.has_focus(id)) {
            if let Some(r) = caret_rect {
                ui.scroll_to_rect(r.expand2(vec2(0.0, 180.0)), Some(Align::Center));
            }
        }

        rect
    }
}

/// Run the text widget and report the geometry the caller needs.
fn run_text_edit(
    ui: &mut Ui,
    buf: &mut String,
    id: Id,
    size: f32,
    bold: bool,
    line_height: f32,
    kind: &BlockKind,
    parsed: &Arc<parser::Parsed>,
    theme: &Theme,
) -> Geometry {
    let mono = matches!(kind, BlockKind::Code(_));
    let family = if mono {
        fonts::mono_family_for(false)
    } else {
        fonts::family_for(bold, false)
    };

    let mut layouter = |ui: &Ui, text: &str, wrap: f32| -> Arc<egui::Galley> {
        let job = source_job(
            text,
            kind,
            parsed,
            theme,
            size,
            line_height,
            bold,
            mono,
            wrap,
        );
        ui.fonts(|f| f.layout_job(job))
    };

    let output = TextEdit::multiline(buf)
        .id(id)
        .frame(false)
        .desired_width(f32::INFINITY)
        .desired_rows(1)
        .margin(egui::Margin::ZERO)
        .font(FontId::new(size, family))
        .layouter(&mut layouter)
        .lock_focus(true)
        .show(ui);

    let caret = output
        .cursor_range
        .map(|cr| output.galley.pos_from_cursor(&cr.primary))
        .map(|r| r.translate(output.galley_pos.to_vec2()));

    Geometry {
        rect: output.response.rect,
        galley_pos: output.galley_pos,
        caret,
        galley: output.galley,
    }
}

/// Geometry of the active block's text widget, reported back by [`run_text_edit`].
struct Geometry {
    rect: Rect,
    galley_pos: Pos2,
    caret: Option<Rect>,
    galley: Arc<egui::Galley>,
}

/// Build the layout job for the raw Markdown of the active block: structural
/// markers are dimmed, inline constructs keep their styling, and fenced code is
/// syntax highlighted while you type in it.
#[allow(clippy::too_many_arguments)]
fn source_job(
    src: &str,
    kind: &BlockKind,
    parsed: &Arc<parser::Parsed>,
    theme: &Theme,
    size: f32,
    line_height: f32,
    bold: bool,
    mono: bool,
    wrap: f32,
) -> LayoutJob {
    let mut job = LayoutJob {
        text: src.to_string(),
        ..Default::default()
    };
    job.wrap.max_width = wrap.max(24.0);

    let base_family = if mono {
        fonts::mono_family_for(false)
    } else {
        fonts::family_for(bold, false)
    };
    let base_color = if mono { theme.code_text } else { theme.text };
    let base = |color: Color32, family: egui::FontFamily| egui::TextFormat {
        font_id: FontId::new(size, family),
        color,
        line_height: Some(size * line_height),
        ..Default::default()
    };
    let base_fmt = base(base_color, base_family.clone());
    let marker_fmt = base(theme.marker, fonts::family_for(false, false));
    let fence_fmt = base(theme.marker, fonts::mono_family_for(false));

    // Pre-highlight fenced code bodies so editing a code block is coloured.
    let code_lines: Option<Vec<Vec<code_hl::Token>>> = match kind {
        BlockKind::Code(cb) if !cb.lang.is_empty() => Some(code_hl::highlight(
            src,
            &cb.lang,
            theme.code_theme(),
            theme.code_text,
        )),
        _ => None,
    };

    let total = src.len();
    let mut covered = 0usize;
    let mut sections: Vec<LayoutSection> = Vec::new();
    let push = |sections: &mut Vec<LayoutSection>, covered: &mut usize, to: usize, fmt: egui::TextFormat| {
        let to = to.min(total);
        if to <= *covered {
            return;
        }
        sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: *covered..to,
            format: fmt,
        });
        *covered = to;
    };

    let mut line_no = 0usize;
    let mut line_start = 0usize;
    while line_start <= total {
        let line_end = match src[line_start..].find('\n') {
            Some(p) => line_start + p,
            None => total,
        };
        let line = &src[line_start..line_end];
        let m = parser::line_marker_len(kind, line_no, line).min(line.len());

        if m > 0 {
            let fmt = if mono { fence_fmt.clone() } else { marker_fmt.clone() };
            push(&mut sections, &mut covered, line_start + m, fmt);
        }

        let rest_start = (line_start + m).min(total);
        let rest = &src[rest_start..line_end];
        let is_body_code = matches!(kind, BlockKind::Code(_)) && code_lines.is_some();

        if rest.is_empty() {
            push(&mut sections, &mut covered, line_end, base_fmt.clone());
        } else if let Some(lines) = &code_lines {
            let hl = lines.get(line_no).cloned().unwrap_or_default();
            let mut p = rest_start;
            for tok in &hl {
                let a = p;
                let b = (p + tok.text.len()).min(line_end);
                let fmt = egui::TextFormat {
                    font_id: FontId::new(size, fonts::mono_family_for(tok.bold)),
                    color: tok.color,
                    italics: tok.italic,
                    line_height: Some(size * line_height),
                    ..Default::default()
                };
                push(&mut sections, &mut covered, a, base_fmt.clone());
                push(&mut sections, &mut covered, b, fmt);
                p = b;
            }
            push(&mut sections, &mut covered, line_end, base_fmt.clone());
        } else if matches!(kind, BlockKind::Math { .. }) {
            let fmt = base(theme.accent, fonts::mono_family_for(false));
            push(&mut sections, &mut covered, rest_start, base_fmt.clone());
            push(&mut sections, &mut covered, line_end, fmt);
        } else if is_body_code {
            push(&mut sections, &mut covered, line_end, base_fmt.clone());
        } else {
            let leaves = parser::parse_inline(
                rest,
                &parser::InlineCtx {
                    defs: &parsed.defs,
                    keep_markers: true,
                },
            );
            for leaf in &leaves {
                match leaf {
                    Leaf::Span {
                        style,
                        link,
                        src: sr,
                        ..
                    } => {
                        // Ranges are source ranges, never `text.len()`: a leaf
                        // shows the source it came from, and escapes (`\$`),
                        // entities (`&amp;`) and autolinks all display fewer
                        // bytes than they occupy, so a text-length offset
                        // drifts and eventually lands inside a multi-byte
                        // character — which is a panic in the shaper.
                        let a = (rest_start + sr.start).min(line_end);
                        let b = (rest_start + sr.end).min(line_end).max(a);
                        let fmt = render::span_format(
                            theme,
                            size,
                            line_height,
                            bold,
                            false,
                            *style,
                            link.as_deref(),
                            base_color,
                        );
                        // The gap before the leaf is plain; the leaf itself
                        // carries its own format.
                        push(&mut sections, &mut covered, a, base_fmt.clone());
                        push(&mut sections, &mut covered, b, fmt);
                    }
                    other => {
                        let sr = other.src();
                        let a = (rest_start + sr.start).min(line_end);
                        let b = (rest_start + sr.end).min(line_end).max(a);
                        let fmt = base(theme.accent, base_family.clone());
                        push(&mut sections, &mut covered, a, base_fmt.clone());
                        push(&mut sections, &mut covered, b, fmt);
                    }
                }
            }
            push(&mut sections, &mut covered, line_end, base_fmt.clone());
        }

        if line_end >= total {
            break;
        }
        push(&mut sections, &mut covered, line_end + 1, base_fmt.clone());
        line_start = line_end + 1;
        line_no += 1;
    }
    push(&mut sections, &mut covered, total, base_fmt.clone());

    if covered < total {
        sections.push(LayoutSection {
            leading_space: 0.0,
            byte_range: covered..total,
            format: base_fmt,
        });
    }
    job.sections = sections;
    job
}

// ===========================================================================
// Helpers
// ===========================================================================

fn rctx_indent(quote: usize, kind: &BlockKind) -> f32 {
    let mut x = quote as f32 * 18.0;
    if let BlockKind::ListItem(li) = kind {
        x += li.depth as f32 * 22.0;
    }
    x
}

// ===========================================================================
// Block heights
// ===========================================================================

/// How tall a block has to be before its height is known for certain.
///
/// Sub-pixel differences are rounding, not information, and correcting for them
/// would shift every block below on almost every frame.
const HEIGHT_TOLERANCE: f32 = 0.5;

/// How many blocks a document may have before its heights are estimated rather
/// than measured.
///
/// Laying every block out up front costs about three microseconds and thirteen
/// kilobytes each, so a document of a few thousand blocks is measured exactly
/// and nothing here is visible at all — which is the case for essentially every
/// real note. Past that the price is a second of work and a gigabyte of laid
/// out text just to open a file, so a large document starts from estimates and
/// becomes exact as it is scrolled through.
const MEASURE_LIMIT: usize = 2048;

/// How much of the font size a character takes up, on average.
///
/// Only used to judge how many lines a block will wrap into, so the numbers are
/// the usual rough ones: Latin text runs about half an em per character, and
/// the wide scripts — CJK and the full-width forms — take a whole one.
fn advance_em(c: char) -> f32 {
    match c as u32 {
        0x1100..=0x115F
        | 0x2E80..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 => 1.0,
        _ if c.is_ascii() => 0.52,
        _ => 0.6,
    }
}

/// Where every block sits vertically, without laying the document out.
///
/// A live-preview document is a list of independent blocks, and laying all of
/// them out is what makes a large file unusable: a hundred thousand blocks cost
/// roughly 300 ms per frame, while the thirty that fit on screen cost about
/// 0.2 ms. Choosing those thirty needs the height of every block above them, so
/// the heights are measured once and then kept.
///
/// The measurement comes from the real layout rather than from an estimate: a
/// frame that does not know the heights yet draws the document in full and
/// records what the layout cursor did, exactly as it did before there was a
/// cache. From then on a frame draws its window, records the height of each
/// block it drew, and hands on to the blocks below any difference — so an image
/// that finishes decoding, or a table that re-wraps, moves the blocks under it
/// and nothing else.
#[derive(Default)]
struct Heights {
    /// Number of blocks described. `each` and `tops` always have this length.
    count: usize,
    /// Column width the heights were measured at: wrapped text is a different
    /// height at a different width, so a change throws all of them away.
    width: f32,
    base: f32,
    line_height: f32,
    /// Vertical space egui leaves between two widgets. Part of the geometry, so
    /// it is remembered alongside it.
    spacing: f32,
    /// Height of every block, excluding the gap above it and the spacing below.
    /// `NaN` means "never laid out".
    each: Vec<f32>,
    /// Byte length of every block, so that after an edit the height of an old
    /// block can be handed to the blocks that replaced it.
    lens: Vec<u32>,
    /// Distance from the top of the document to the top of each block, gaps and
    /// spacing included. Non-decreasing, which is what makes it searchable.
    tops: Vec<f32>,
    /// Distance from the top of the document to the bottom of the last block.
    total: f32,
    /// How many entries of `each` are still `NaN`.
    unknown: usize,
}

impl Heights {
    /// Bring the cache in line with the document and the column. Cheap when
    /// nothing has changed, which is the usual case.
    fn prepare(
        &mut self,
        parsed: &parser::Parsed,
        col_w: f32,
        base: f32,
        line_height: f32,
        spacing: f32,
    ) {
        let n = parsed.blocks.len();
        let reflowed = (self.width - col_w).abs() > HEIGHT_TOLERANCE
            || (self.base - base).abs() > 0.01
            || (self.line_height - line_height).abs() > 0.001
            || (self.spacing - spacing).abs() > 0.001;
        if self.count == n && !reflowed {
            return;
        }

        if reflowed || self.count == 0 {
            // Nothing measured at one width says anything about another.
            self.each.clear();
            self.lens.clear();
        } else {
            self.hand_over(parsed);
        }

        self.count = n;
        self.width = col_w;
        self.base = base;
        self.line_height = line_height;
        self.spacing = spacing;

        self.each.resize(n, f32::NAN);
        // A document small enough to draw in one go is measured exactly; a
        // large one starts from guesses and is corrected as it is drawn.
        if n > MEASURE_LIMIT && self.each.iter().any(|h| h.is_nan()) {
            let mut filled = Vec::with_capacity(n);
            for i in 0..n {
                let known = self.each[i];
                filled.push(if known.is_nan() {
                    self.estimate(parsed, i)
                } else {
                    known
                });
            }
            self.each = filled;
        }
        self.lens.clear();
        self.lens.extend(
            parsed
                .blocks
                .iter()
                .map(|b| b.range.len().min(u32::MAX as usize) as u32),
        );
        self.rewrap(parsed);
        self.unknown = self.each.iter().filter(|h| h.is_nan()).count();
    }

    /// A first guess at a block's height, used until it has been drawn once.
    ///
    /// The guess decides how far a scrollbar or an outline jump lands from the
    /// truth before the block has been seen, so it is worked out from the
    /// block's own text and the column it has to fit into rather than from a
    /// constant.
    fn estimate(&self, parsed: &parser::Parsed, i: usize) -> f32 {
        let block = &parsed.blocks[i];
        let em = self.base.max(1.0);
        let (size, line_height) = match &block.kind {
            BlockKind::Heading { level } => (render::heading_size(em, *level), 1.34),
            BlockKind::Code(_) => (em * 0.88, 1.62),
            _ => (em, self.line_height),
        };

        // A blank line is shorter than a line of text.
        if matches!(block.kind, BlockKind::Blank) {
            return em * self.line_height * 0.8;
        }

        let width = self.width.max(120.0);
        let mut lines = 0.0f32;
        for line in block.content.split('\n') {
            let advance: f32 = line.chars().map(|c| advance_em(c) * size).sum();
            lines += (advance / width).ceil().max(1.0);
        }

        let chrome = match &block.kind {
            // Drawn inside a frame, with padding above and below.
            BlockKind::Code(_) | BlockKind::Table { .. } => size * 0.9,
            BlockKind::Math { .. } => size * 0.3,
            BlockKind::Rule => size * 0.4,
            BlockKind::LinkDef { .. } | BlockKind::FootnoteDef { .. } => size * 0.2,
            _ => 0.0,
        };
        lines.max(1.0) * size * line_height + chrome
    }

    /// Whether every height has been measured, and the window can be trusted.
    fn complete(&self) -> bool {
        self.unknown == 0
    }

    /// Give each new block the share of the old block it came from.
    ///
    /// An edit that splits or joins blocks changes how many there are, but most
    /// of them are untouched and their heights are still right. Only the byte
    /// spans matter: a paragraph split in two keeps half its height each, so
    /// the document stays the same size and the blocks that were never touched
    /// do not move on screen.
    fn hand_over(&mut self, parsed: &parser::Parsed) {
        if self.count == 0 || self.each.len() != self.count || self.lens.len() != self.count {
            return;
        }
        let old_heights = std::mem::take(&mut self.each);
        let old_lens = std::mem::take(&mut self.lens);
        let old_count = self.count;

        let mut out = Vec::with_capacity(parsed.blocks.len());
        let mut k = 0usize;
        let mut old_start = 0u32;
        for block in &parsed.blocks {
            let start = block.range.start.min(u32::MAX as usize) as u32;
            // Walk to the old block that used to contain this byte offset.
            while k + 1 < old_count && old_start.saturating_add(old_lens[k]) <= start {
                old_start = old_start.saturating_add(old_lens[k]);
                k += 1;
            }
            let old_len = old_lens[k].max(1);
            let new_len = (block.range.len().min(u32::MAX as usize) as u32).max(1);
            let share = (new_len as f32 / old_len as f32).clamp(0.15, 4.0);
            out.push(old_heights[k] * share);
        }
        self.each = out;
    }

    /// Recompute `tops` and `total` from the heights.
    ///
    /// Runs on the frames where something changed, never on the common frame.
    fn rewrap(&mut self, parsed: &parser::Parsed) {
        self.tops.clear();
        self.tops.reserve(self.count);
        let mut y = 0.0f32;
        for i in 0..self.count {
            y += gap_before(parsed, i, self.base);
            self.tops.push(y);
            y += self.height(i) + self.spacing;
        }
        self.total = y;
    }

    /// Height of block `i`, with "never laid out" counting as nothing.
    fn height(&self, i: usize) -> f32 {
        self.each.get(i).copied().filter(|h| !h.is_nan()).unwrap_or(0.0)
    }

    /// Distance from the top of the document to the top of block `i`.
    fn top(&self, i: usize) -> f32 {
        self.tops.get(i).copied().unwrap_or(0.0)
    }

    fn total(&self) -> f32 {
        self.total
    }

    /// Record what a block actually measured, and move the blocks below it.
    ///
    /// This is the only place the cache learns anything, and it is fed by the
    /// real layout, so the model cannot drift away from what is drawn without
    /// the next frame correcting it.
    fn record(&mut self, i: usize, height: f32) {
        if i >= self.count {
            return;
        }
        let old = self.each[i];
        let known = !old.is_nan();
        if known && (old - height).abs() <= HEIGHT_TOLERANCE {
            return;
        }
        let delta = height - if known { old } else { 0.0 };
        self.each[i] = height;
        if !known {
            self.unknown = self.unknown.saturating_sub(1);
        }
        if delta != 0.0 {
            for t in &mut self.tops[i + 1..] {
                *t += delta;
            }
            self.total += delta;
        }
    }

    /// Called once the frame that drew the whole document is over.
    fn mark_measured(&mut self, n: usize) {
        if n == self.count {
            self.unknown = 0;
        }
    }

    /// The blocks to draw for a viewport, plus a little either side so that a
    /// fast scroll does not show a band of empty space.
    fn window(&self, viewport: Rect, n: usize, plan: &mut Vec<usize>) {
        plan.clear();
        if n == 0 || self.tops.len() != n {
            return;
        }
        const OVERSCAN: usize = 2;
        let first = self
            .tops
            .partition_point(|&t| t < viewport.min.y)
            .saturating_sub(1 + OVERSCAN);
        let last = (self
            .tops
            .partition_point(|&t| t <= viewport.max.y)
            + OVERSCAN)
            .min(n)
            .max((first + 1).min(n));
        plan.extend(first..last);
    }

    /// The scroll offset that centres block `i`, if the heights are known.
    fn offset_for(&self, i: usize, viewport_h: f32) -> Option<f32> {
        if i >= self.count || self.unknown > 0 {
            return None;
        }
        let centre = self.tops[i] + self.each[i] * 0.5 - viewport_h * 0.5;
        let furthest = (self.total - viewport_h).max(0.0);
        Some(centre.clamp(0.0, furthest))
    }
}

fn gap_before(parsed: &parser::Parsed, i: usize, base: f32) -> f32 {
    if i == 0 {
        return 0.0;
    }
    let em = base;
    let prev = &parsed.blocks[i - 1].kind;
    let cur = &parsed.blocks[i].kind;
    use BlockKind::*;
    let is_block = |k: &BlockKind| matches!(k, Code(_) | Table { .. } | Rule | Math { .. });
    if matches!(prev, Blank) || matches!(cur, Blank) {
        return match cur {
            Heading { .. } => em * 0.35,
            _ => 0.0,
        };
    }
    if is_block(prev) || is_block(cur) {
        return em * 0.62;
    }
    match (prev, cur) {
        (Heading { .. }, _) => em * 0.34,
        (_, Heading { .. }) => em * 0.6,
        (ListItem(_), ListItem(_)) => em * 0.14,
        (ListItem(_), _) | (_, ListItem(_)) => em * 0.5,
        _ => em * 0.36,
    }
}

/// Content offset for a click, choosing the hit region the pointer is closest to.
fn nearest_content_offset(hits: &[Hit], pos: Pos2, block: &parser::Block) -> usize {
    if hits.is_empty() {
        return block.content.len();
    }
    let mut best: Option<&Hit> = None;
    let mut best_d = f32::INFINITY;
    for h in hits {
        if h.rect.contains(pos) {
            best = Some(h);
            break;
        }
        let dy = if pos.y < h.rect.top() {
            h.rect.top() - pos.y
        } else if pos.y > h.rect.bottom() {
            pos.y - h.rect.bottom()
        } else {
            0.0
        };
        let dx = if pos.x < h.rect.left() {
            h.rect.left() - pos.x
        } else if pos.x > h.rect.right() {
            pos.x - h.rect.right()
        } else {
            0.0
        };
        // Prefer staying on the same visual line.
        let d = dy * 10.0 + dx;
        if d < best_d {
            best_d = d;
            best = Some(h);
        }
    }
    match best {
        Some(h) => {
            let clamped = Pos2::new(
                pos.x.clamp(h.rect.left(), h.rect.right()),
                pos.y.clamp(h.rect.top(), h.rect.bottom()),
            );
            h.content_offset(clamped)
        }
        None => block.content.len(),
    }
}

/// For blocks without text hit regions (code, tables, rules), derive a source
/// offset from the vertical position inside the block.
fn line_offset_from_y(
    doc: &Document,
    ec: &EditorCtx,
    idx: usize,
    pos: Pos2,
    block_rect: Rect,
) -> usize {
    let Some(block) = doc.parsed.blocks.get(idx) else {
        return 0;
    };
    let r = block.edit_range(&doc.text);
    let line_h = match &block.kind {
        BlockKind::Code(_) => ec.base * 0.88 * 1.62,
        BlockKind::Rule => ec.base * 0.5,
        _ => ec.base * ec.line_height,
    }
    .max(6.0);
    // The click's y is relative to the block row, which starts at the block top.
    let row = ((pos.y - block_rect.top()) / line_h).floor().max(0.0) as usize;
    let mut off = r.start;
    for (k, l) in doc.text[r.clone()].split('\n').enumerate() {
        if k >= row {
            break;
        }
        off += l.len() + 1;
    }
    off.min(r.end)
}

fn link_at(doc: &Document, idx: usize, off: usize) -> Option<String> {
    let block = doc.parsed.blocks.get(idx)?;
    let leaves = parser::parse_inline(
        &block.content,
        &parser::InlineCtx {
            defs: &doc.parsed.defs,
            keep_markers: false,
        },
    );
    for l in &leaves {
        if let Leaf::Span {
            link: Some(url),
            src,
            ..
        } = l
        {
            if src.contains(&off) || src.start == off {
                return Some(url.clone());
            }
        }
    }
    None
}

fn push_caret(ctx: &egui::Context, id: Id, buf: &str, anchor: usize, cursor: usize) {
    let a = char_index_from_byte(buf, anchor.min(buf.len()));
    let b = char_index_from_byte(buf, cursor.min(buf.len()));
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(CCursor::new(a), CCursor::new(b))));
    state.store(ctx, id);
}

fn push_caret_at(ctx: &egui::Context, id: Id, index: usize) {
    let mut state = TextEditState::load(ctx, id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::two(
            CCursor::new(index),
            CCursor::new(index),
        )));
    state.store(ctx, id);
}

fn char_index_from_byte(s: &str, byte: usize) -> usize {
    s[..byte.min(s.len())].chars().count()
}

fn char_index_to_byte(s: &str, idx: usize) -> usize {
    s.char_indices()
        .nth(idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

fn prev_boundary(s: &str, at: usize) -> usize {
    let at = at.min(s.len());
    if at == 0 {
        return 0;
    }
    let mut p = at - 1;
    while p > 0 && !s.is_char_boundary(p) {
        p -= 1;
    }
    p
}

/// Open a URL (or a relative path) with the platform's default handler.
pub fn open_url(url: &str) {
    let url = url.trim();
    if url.is_empty() {
        return;
    }
    let target = if url.starts_with("http://")
        || url.starts_with("https://")
        || url.starts_with("mailto:")
        || url.starts_with("file://")
    {
        url.to_string()
    } else if url.contains('@') && !url.contains('/') {
        format!("mailto:{url}")
    } else {
        let p = std::path::PathBuf::from(url);
        let abs = std::fs::canonicalize(&p).unwrap_or(p);
        format!("file://{}", abs.display())
    };
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(target_os = "windows")]
    let cmd = "cmd";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new(cmd).args(["/C", "start", "", &target]).spawn();
    #[cfg(not(target_os = "windows"))]
    let _ = std::process::Command::new(cmd).arg(target).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` blocks with known heights, as if a frame had drawn them all.
    fn measured(text: &str, height: f32) -> Heights {
        let parsed = parser::parse(text);
        let mut h = Heights::default();
        h.prepare(&parsed, 700.0, 16.0, 1.7, 3.0);
        for i in 0..h.count {
            h.record(i, height);
        }
        h
    }

    fn tops_agree_with_heights(h: &Heights, parsed: &parser::Parsed) -> bool {
        let mut y = 0.0f32;
        for i in 0..h.count {
            y += gap_before(parsed, i, h.base);
            if (h.top(i) - y).abs() > 0.01 {
                return false;
            }
            y += h.each[i] + h.spacing;
        }
        (h.total - y).abs() <= 0.01
    }

    #[test]
    fn measured_heights_add_up_to_positions() {
        let text = "甲\n\n乙\n\n- 丙\n\n```\ncode\n```\n";
        let parsed = parser::parse(text);
        let h = measured(text, 20.0);
        assert_eq!(h.count, parsed.blocks.len());
        assert!(h.complete(), "every height was recorded");
        assert!(h.tops.windows(2).all(|w| w[0] <= w[1]), "tops are sorted");
        assert!(tops_agree_with_heights(&h, &parsed));
    }

    #[test]
    fn a_taller_block_pushes_the_rest_down() {
        let text = "甲\n\n乙\n\n丙\n";
        let parsed = parser::parse(text);
        let mut h = measured(text, 20.0);
        let before: Vec<f32> = h.tops.clone();
        let total_before = h.total;

        h.record(0, 50.0);

        assert_eq!(h.top(0), before[0], "the block itself does not move");
        assert!(
            (h.top(1) - before[1] - 30.0).abs() < 0.01,
            "everything below moves by the difference"
        );
        assert!((h.total - total_before - 30.0).abs() < 0.01);
        assert!(tops_agree_with_heights(&h, &parsed));
    }

    #[test]
    fn a_rounding_difference_is_left_alone() {
        let h = measured("甲\n\n乙\n", 20.0);
        let mut h = h;
        h.record(0, 20.2);
        assert_eq!(h.each[0], 20.0, "half a pixel is rounding, not news");
    }

    #[test]
    fn the_window_covers_the_viewport() {
        let text = "甲\n\n乙\n\n丙\n\n丁\n\n戊\n";
        let h = measured(text, 100.0);

        // Fully inside the document: the window has to hold every block that
        // overlaps the viewport, not just the first one.
        let viewport = Rect::from_min_size(Pos2::new(0.0, h.top(2)), vec2(700.0, 120.0));
        let mut plan = Vec::new();
        h.window(viewport, h.count, &mut plan);
        let first = *plan.first().expect("a window");
        let last = *plan.last().expect("a window");
        assert!(first <= 2 && last >= 2, "block 2 is on screen: {plan:?}");
        assert!(plan.windows(2).all(|w| w[0] < w[1]), "in order, no repeats");

        // Past the end, and before the start, still return blocks to draw.
        for y in [-500.0, h.total + 500.0] {
            let vp = Rect::from_min_size(Pos2::new(0.0, y), vec2(700.0, 120.0));
            h.window(vp, h.count, &mut plan);
            assert!(!plan.is_empty(), "a document always draws something");
            assert!(*plan.last().unwrap() <= h.count);
        }
    }

    #[test]
    fn a_split_block_hands_its_height_to_both_halves() {
        // One paragraph, then the same text split in two by a blank line: the
        // blocks are new, but the space they take up should be about the same.
        let mut h = measured("abcdefghij\n", 44.0);
        let parsed = parser::parse("abcde\n\nfghij\n");
        h.prepare(&parsed, 700.0, 16.0, 1.7, 3.0);

        assert_eq!(h.count, parsed.blocks.len());
        let blanks = parsed
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::Blank))
            .count();
        let paragraphs: Vec<f32> = parsed
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b.kind, BlockKind::Paragraph))
            .map(|(i, _)| h.each[i])
            .collect();
        assert_eq!(paragraphs.len(), 2, "the split made two paragraphs");
        // Named ranges include the trailing newline, so each half is six of the
        // original eleven bytes, and a bit over half the height.
        for got in paragraphs {
            assert!(
                (got - 24.0).abs() < 2.0,
                "expected about 24 px from a 44 px block, got {got}"
            );
        }
        assert_eq!(blanks, 1);
        assert!(h.complete(), "every block has a height to work from");
    }

    #[test]
    fn an_unknown_height_makes_the_document_incomplete() {
        let parsed = parser::parse("甲\n\n乙\n");
        let mut h = Heights::default();
        h.prepare(&parsed, 700.0, 16.0, 1.7, 3.0);
        // Small enough to measure exactly, so nothing is guessed and the frame
        // that follows has to draw the whole document.
        assert!(!h.complete());
        assert!(h.offset_for(1, 400.0).is_none());
    }

    #[test]
    fn a_large_document_is_guessed_at_rather_than_laid_out() {
        // A little over the measuring limit, all one-line paragraphs.
        let text: String = (0..MEASURE_LIMIT + 50).map(|i| format!("段落 {i}\n\n")).collect();
        let parsed = parser::parse(&text);
        let mut h = Heights::default();
        h.prepare(&parsed, 700.0, 16.0, 1.7, 3.0);
        assert!(
            h.complete(),
            "a document this size starts from estimates, not a full layout"
        );
        assert!(h.total > 0.0);
        assert!(h.tops.windows(2).all(|w| w[0] <= w[1]));
        // The guess is in the right parish: a paragraph at 16 px with 1.7 line
        // spacing is somewhere around 27 px, not 3 and not 300.
        let one = h.each.iter().copied().find(|h| *h > 0.0).unwrap();
        assert!((10.0..60.0).contains(&one), "estimated {one} px per line");
    }

    #[test]
    fn a_jump_lands_within_the_document() {
        let h = measured("甲\n\n乙\n\n丙\n", 100.0);
        let viewport = 200.0;
        let start = h.offset_for(0, viewport).expect("heights are known");
        assert_eq!(start, 0.0, "the first block cannot be centred");
        let last = h.offset_for(h.count - 1, viewport).expect("heights are known");
        assert!(
            last <= (h.total - viewport).max(0.0) + 0.01,
            "never scrolls past the end"
        );
        // A block in the middle is centred, not just visible.
        let mid = h.offset_for(2, viewport).unwrap();
        assert!((mid - (h.top(2) + 50.0 - 100.0)).abs() < 0.01);
    }
}
