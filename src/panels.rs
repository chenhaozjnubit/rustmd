//! Side panels and chrome: menu bar, file tree, outline, find & replace and the
//! status bar. All of these mutate the application directly.

use std::path::{Path, PathBuf};

use egui::{Align, Color32, FontId, RichText, ScrollArea, Sense, TextEdit, Ui, vec2};

use crate::app::{App, Pane};
use crate::fonts;
use crate::picker;

// ===========================================================================
// Menu bar
// ===========================================================================

pub fn menu_bar(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.menu_button("文件", |ui| {
            if ui.button("新建          ⌘N").clicked() {
                app.new_document();
                ui.close_menu();
            }
            ui.separator();
            // Our own browser is listed first on purpose: it filters to
            // Markdown, it is keyboard driven, and unlike the OS panel it can
            // never grey out the file you came here to open.
            if ui.button("打开…         ⌘O").clicked() {
                app.open_dialog();
                ui.close_menu();
            }
            if ui.button("用系统对话框打开…   ⇧⌘O").clicked() {
                app.open_system_dialog();
                ui.close_menu();
            }
            if ui.button("打开文件夹…").clicked() {
                app.open_folder_dialog();
                ui.close_menu();
            }
            ui.menu_button("最近打开", |ui| {
                if app.cfg.recent.is_empty() {
                    ui.add_enabled(false, egui::Button::new("（暂无记录）"));
                    return;
                }
                let recent = app.cfg.recent.clone();
                for p in recent {
                    let label = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.display().to_string());
                    if ui
                        .button(label)
                        .on_hover_text(p.display().to_string())
                        .clicked()
                    {
                        app.open_path(&p);
                        ui.close_menu();
                    }
                }
            });
            ui.separator();
            if ui.button("保存          ⌘S").clicked() {
                app.save();
                ui.close_menu();
            }
            if ui.button("另存为…       ⇧⌘S").clicked() {
                app.save_as_dialog();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("导出为 HTML…").clicked() {
                app.export_html_dialog();
                ui.close_menu();
            }
            if ui.button("复制全文 Markdown").clicked() {
                app.copy_all();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("重新载入").clicked() {
                app.reload();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("退出").clicked() {
                app.request_quit();
                ui.close_menu();
            }
        });

        ui.menu_button("编辑", |ui| {
            let can_undo = app.doc.can_undo();
            let can_redo = app.doc.can_redo();
            if ui
                .add_enabled(can_undo, egui::Button::new("撤销          ⌘Z"))
                .clicked()
            {
                app.undo();
                ui.close_menu();
            }
            if ui
                .add_enabled(can_redo, egui::Button::new("重做          ⇧⌘Z"))
                .clicked()
            {
                app.redo();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("全选当前块    ⌘A").clicked() {
                app.select_all();
                ui.close_menu();
            }
            if ui.button("查找 / 替换   ⌘F").clicked() {
                app.find.open = true;
                app.find.focus_query = true;
                ui.close_menu();
            }
        });

        ui.menu_button("格式", |ui| {
            if ui.button("加粗          ⌘B").clicked() {
                app.wrap_selection("**", "**");
                ui.close_menu();
            }
            if ui.button("斜体          ⌘I").clicked() {
                app.wrap_selection("*", "*");
                ui.close_menu();
            }
            if ui.button("删除线").clicked() {
                app.wrap_selection("~~", "~~");
                ui.close_menu();
            }
            if ui.button("行内代码      ⌘E").clicked() {
                app.wrap_selection("`", "`");
                ui.close_menu();
            }
            if ui.button("高亮").clicked() {
                app.wrap_selection("==", "==");
                ui.close_menu();
            }
            if ui.button("行内公式").clicked() {
                app.wrap_selection("$", "$");
                ui.close_menu();
            }
            ui.separator();
            ui.menu_button("标题", |ui| {
                for level in 1..=6u8 {
                    if ui.button(format!("{} 级标题", level)).clicked() {
                        app.set_heading(level);
                        ui.close_menu();
                    }
                }
                if ui.button("正文").clicked() {
                    app.set_heading(0);
                    ui.close_menu();
                }
            });
            if ui.button("引用").clicked() {
                app.prefix_lines("> ");
                ui.close_menu();
            }
            if ui.button("无序列表").clicked() {
                app.prefix_lines("- ");
                ui.close_menu();
            }
            if ui.button("有序列表").clicked() {
                app.prefix_lines("1. ");
                ui.close_menu();
            }
            if ui.button("任务列表").clicked() {
                app.prefix_lines("- [ ] ");
                ui.close_menu();
            }
            if ui.button("缩进 +2").clicked() {
                app.prefix_lines("  ");
                ui.close_menu();
            }
            ui.separator();
            if ui.button("插入代码块").clicked() {
                app.insert_block("```\ncode here\n```");
                ui.close_menu();
            }
            if ui.button("插入表格").clicked() {
                app.insert_block("| 列 1 | 列 2 |\n| --- | --- |\n|  |  |");
                ui.close_menu();
            }
            if ui.button("插入分割线").clicked() {
                app.insert_block("---");
                ui.close_menu();
            }
            if ui.button("插入公式块").clicked() {
                app.insert_block("$$\n\\sum_{i=1}^{n} x_i\n$$");
                ui.close_menu();
            }
            if ui.button("插入链接").clicked() {
                app.wrap_selection("[", "](https://)");
                ui.close_menu();
            }
            if ui.button("插入图片").clicked() {
                app.wrap_selection("![", "](image.png)");
                ui.close_menu();
            }
        });

        ui.menu_button("视图", |ui| {
            if ui
                .selectable_label(app.pane() == Pane::Edit, "实时预览")
                .clicked()
            {
                app.set_pane(Pane::Edit);
                ui.close_menu();
            }
            if ui
                .selectable_label(app.pane() == Pane::Source, "源码")
                .clicked()
            {
                app.set_pane(Pane::Source);
                ui.close_menu();
            }
            if ui
                .selectable_label(app.pane() == Pane::Split, "分栏")
                .clicked()
            {
                app.set_pane(Pane::Split);
                ui.close_menu();
            }
            if ui.button("循环切换视图  ⌘⇧E").clicked() {
                app.toggle_pane();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("阅读模式      ⌘R").clicked() {
                app.toggle_reading();
                ui.close_menu();
            }
            ui.separator();
            if ui.button("浅色 / 深色   ⌘/").clicked() {
                app.cfg.theme = app.cfg.theme.toggled();
                ui.close_menu();
            }
            if ui
                .checkbox(&mut app.cfg.show_sidebar, "显示文件树")
                .clicked()
            {
                ui.close_menu();
            }
            if ui.checkbox(&mut app.cfg.show_outline, "显示大纲").clicked() {
                ui.close_menu();
            }
            ui.separator();
            if ui.checkbox(&mut app.cfg.focus_mode, "专注模式").clicked() {
                ui.close_menu();
            }
            if ui.checkbox(&mut app.cfg.typewriter, "打字机模式").clicked() {
                ui.close_menu();
            }
            if ui.checkbox(&mut app.cfg.wrap_code, "代码块自动换行").clicked() {
                ui.close_menu();
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("字号");
                if ui.small_button("−").clicked() {
                    app.cfg.font_size = (app.cfg.font_size - 0.5).clamp(11.0, 30.0);
                }
                ui.label(format!("{:.1}", app.cfg.font_size));
                if ui.small_button("+").clicked() {
                    app.cfg.font_size = (app.cfg.font_size + 0.5).clamp(11.0, 30.0);
                }
            });
            ui.horizontal(|ui| {
                ui.label("版心");
                if ui.small_button("−").clicked() {
                    app.cfg.content_width = (app.cfg.content_width - 40.0).clamp(420.0, 1600.0);
                }
                ui.label(format!("{}", app.cfg.content_width as i32));
                if ui.small_button("+").clicked() {
                    app.cfg.content_width = (app.cfg.content_width + 40.0).clamp(420.0, 1600.0);
                }
            });
            ui.horizontal(|ui| {
                ui.label("行距");
                if ui.small_button("−").clicked() {
                    app.cfg.line_height = (app.cfg.line_height - 0.06).clamp(1.2, 2.6);
                }
                ui.label(format!("{:.2}", app.cfg.line_height));
                if ui.small_button("+").clicked() {
                    app.cfg.line_height = (app.cfg.line_height + 0.06).clamp(1.2, 2.6);
                }
            });
        });

        ui.menu_button("帮助", |ui| {
            if ui.button("快捷键").clicked() {
                app.show_help = true;
                ui.close_menu();
            }
            if ui.button("关于 rustmd").clicked() {
                app.show_about = true;
                ui.close_menu();
            }
        });

        // document title, right aligned
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            let name = app.doc.display_name();
            let dirty = if app.doc.dirty { " ●" } else { "" };
            let label = RichText::new(format!("{name}{dirty}"))
                .font(FontId::new(13.0, fonts::family_for(true, false)))
                .color(app.theme.text_muted);
            ui.label(label).on_hover_text(
                app.doc
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "尚未保存".into()),
            );
        });
    });
}

// ===========================================================================
// File tree
// ===========================================================================

pub fn file_tree(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("文件")
                .font(FontId::new(12.0, fonts::family_for(true, false)))
                .color(app.theme.text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("📂").on_hover_text("打开文件夹").clicked() {
                app.open_folder_dialog();
            }
            if ui.small_button("⟳").on_hover_text("刷新").clicked() {
                app.refresh_tree();
            }
        });
    });
    ui.add_space(4.0);
    ui.separator();

    let Some(root) = app.tree_root.clone() else {
        // Cold start: nothing is open, so the panel has to offer every way in
        // rather than just an instruction.
        ui.add_space(10.0);
        ui.label(
            RichText::new("未打开文件夹")
                .color(app.theme.text_faint)
                .size(12.5),
        );
        ui.add_space(6.0);
        if ui.button("打开文件…").clicked() {
            app.open_dialog();
        }
        if ui.button("选择文件夹…").clicked() {
            app.open_folder_dialog();
        }

        let recent: Vec<PathBuf> = app.cfg.recent.clone();
        if !recent.is_empty() {
            ui.add_space(10.0);
            ui.separator();
            ui.label(
                RichText::new("最近打开")
                    .font(FontId::new(11.5, fonts::family_for(true, false)))
                    .color(app.theme.text_muted),
            );
            ui.add_space(2.0);
            for p in recent {
                let label = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                let resp = row_label(
                    ui,
                    RichText::new(label).size(12.5).color(app.theme.text_muted),
                    p.display().to_string(),
                );
                if resp.clicked() {
                    app.open_path(&p);
                }
            }
        }
        return;
    };

    ui.add(
        egui::Label::new(
            RichText::new(
                root.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string()),
            )
            .size(12.0)
            .color(app.theme.text_faint),
        )
        .truncate(),
    );
    ui.add_space(2.0);

    let nodes = app.tree.clone();
    ScrollArea::vertical()
        .id_salt("filetree")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for n in &nodes {
                tree_node(ui, app, n, 0);
            }
        });
}

fn tree_node(ui: &mut Ui, app: &mut App, node: &crate::app::TreeNode, depth: usize) {
    let theme = app.theme.clone();
    let is_current = app
        .doc
        .path
        .as_ref()
        .map(|p| p == &node.path)
        .unwrap_or(false);

    if node.is_dir {
        let expanded = !app.collapsed.contains(&node.path);
        let mut clicked = false;
        let mut arrow = String::new();
        ui.horizontal(|ui| {
            indent(ui, depth, 0.0);
            let r = ui.allocate_exact_size(vec2(14.0, 16.0), Sense::click());
            arrow = if expanded { "▾".into() } else { "▸".into() };
            ui.painter().text(
                r.0.center(),
                egui::Align2::CENTER_CENTER,
                &arrow,
                FontId::new(11.0, fonts::family_for(false, false)),
                theme.text_faint,
            );
            let label = RichText::new(&node.name).size(13.0).color(theme.text_muted);
            let resp = row_label(ui, label, node.path.display().to_string());
            if resp.clicked() || r.1.clicked() {
                clicked = true;
            }
        });
        if clicked {
            if expanded {
                app.collapsed.insert(node.path.clone());
            } else {
                app.collapsed.remove(&node.path);
            }
        }
        if expanded {
            for c in &node.children {
                tree_node(ui, app, c, depth + 1);
            }
        }
        return;
    }

    ui.horizontal(|ui| {
        indent(ui, depth, 14.0);
        // Markdown is what this program is for; other text files are listed too
        // but drawn more quietly so the useful ones stand out.
        let color = if is_current {
            theme.accent
        } else if picker::is_markdown(&node.path) {
            theme.text
        } else {
            theme.text_muted
        };
        let label = RichText::new(&node.name).size(13.0).color(color);
        let resp = row_label(ui, label, node.path.display().to_string());
        if resp.clicked() {
            let p = node.path.clone();
            app.open_path(&p);
        }
        if resp.double_clicked() {
            let p = node.path.clone();
            app.open_path(&p);
        }
    });
}

/// Indent a tree row, but never so far that the label loses all its room.
///
/// A very deep tree would otherwise push the row past the panel's edge, which
/// is the same failure `[`row_label`] guards against.
fn indent(ui: &mut Ui, depth: usize, extra: f32) {
    let want = depth as f32 * 12.0 + extra;
    let room = (ui.available_width() - 40.0).max(0.0);
    ui.add_space(want.min(room));
}

/// A clickable row label that can never widen its panel.
///
/// A `Label` with no wrap mode *extends* instead of wrapping, and a panel's
/// background is painted over its content's used rect. So one long file name
/// made the sidebar's fill stop at the panel edge while the panel still
/// reserved the wider rect — the space from the edge to the reserved rect was
/// painted by nobody, and showed up as a black bar (the window background
/// through the transparent clear). Truncating keeps every row inside the panel;
/// the full name is still available on hover.
fn row_label(ui: &mut Ui, text: RichText, tip: String) -> egui::Response {
    ui.add(egui::Label::new(text).sense(Sense::click()).truncate())
        .on_hover_text(tip)
}

// ===========================================================================
// Outline
// ===========================================================================

pub fn outline(ui: &mut Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("大纲")
                .font(FontId::new(12.0, fonts::family_for(true, false)))
                .color(app.theme.text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{:.0}%", app.editor.scroll_percent * 100.0))
                    .size(11.0)
                    .color(app.theme.text_faint),
            );
        });
    });
    ui.add_space(4.0);
    ui.separator();

    let items = app.doc.outline();
    if items.is_empty() {
        ui.add_space(10.0);
        ui.label(
            RichText::new("尚无标题")
                .color(app.theme.text_faint)
                .size(12.5),
        );
        return;
    }

    let theme = app.theme.clone();
    let active = app.editor.active.map(|_| app.editor.cursor);
    ScrollArea::vertical()
        .id_salt("outline")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            let mut jump: Option<usize> = None;
            for item in &items {
                let is_active = active
                    .map(|c| c >= item.offset && c < item.offset + 200)
                    .unwrap_or(false);
                ui.horizontal(|ui| {
                    indent(ui, usize::from(item.level.saturating_sub(1)), 0.0);
                    let color = if is_active {
                        theme.accent
                    } else if item.level <= 2 {
                        theme.text
                    } else {
                        theme.text_muted
                    };
                    let text = if item.text.is_empty() {
                        "(无标题)".to_string()
                    } else {
                        item.text.clone()
                    };
                    let size = match item.level {
                        1 => 13.5,
                        2 => 13.0,
                        _ => 12.5,
                    };
                    let resp = ui.add(
                        egui::Label::new(RichText::new(text).size(size).color(color))
                            .sense(Sense::click())
                            .truncate(),
                    );
                    if resp.clicked() {
                        jump = Some(item.block);
                    }
                });
            }
            if let Some(block) = jump {
                app.jump_to_block(block);
            }
        });
}

// ===========================================================================
// Find & replace
// ===========================================================================

pub fn find_bar(ui: &mut Ui, app: &mut App) {
    let theme = app.theme.clone();
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("查找")
                .size(12.5)
                .color(theme.text_muted),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(&mut app.find.query)
                .desired_width(160.0)
                .hint_text("搜索文本"),
        );
        if app.find.focus_query {
            resp.request_focus();
            app.find.focus_query = false;
        }
        if resp.changed() {
            app.recompute_matches();
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            app.find_next();
        }

        let count = if app.find.matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", app.find.current + 1, app.find.matches.len())
        };
        ui.label(RichText::new(count).size(12.0).color(theme.text_faint));

        if ui.small_button("◀").on_hover_text("上一个").clicked() {
            app.find_prev();
        }
        if ui.small_button("▶").on_hover_text("下一个").clicked() {
            app.find_next();
        }

        ui.separator();
        ui.label(RichText::new("替换").size(12.5).color(theme.text_muted));
        ui.add(
            egui::TextEdit::singleline(&mut app.find.replace)
                .desired_width(150.0)
                .hint_text("替换为"),
        );
        if ui.small_button("替换").clicked() {
            app.replace_current();
        }
        if ui.small_button("全部").clicked() {
            app.replace_all();
        }

        ui.separator();
        ui.checkbox(&mut app.find.case_sensitive, "区分大小写");
        ui.checkbox(&mut app.find.whole_word, "全词");

        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("✕").on_hover_text("关闭").clicked() {
                app.find.open = false;
            }
        });
    });
}

// ===========================================================================
// Status bar
// ===========================================================================

pub fn status_bar(ui: &mut Ui, app: &mut App) {
    let theme = app.theme.clone();
    let stats = app.doc.stats();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;

        let mode = match (&app.pane(), app.editor.reading) {
            (Pane::Edit, false) => "编辑",
            (Pane::Edit, true) => "阅读",
            (Pane::Source, _) => "源码",
            (Pane::Split, _) => "分栏",
        };
        ui.label(RichText::new(mode).size(12.0).color(theme.accent));

        if let Some(msg) = &app.toast {
            ui.label(RichText::new(&msg.0).size(12.0).color(theme.text_muted));
        }

        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            ui.label(
                RichText::new(format!("{:.1} 分钟阅读", stats.reading_minutes))
                    .size(12.0)
                    .color(theme.text_faint),
            );
            ui.label(
                RichText::new(format!("{} 行", stats.lines))
                    .size(12.0)
                    .color(theme.text_faint),
            );
            ui.label(
                RichText::new(format!("{} 字", stats.words))
                    .size(12.0)
                    .color(theme.text_faint),
            );
            if app.cfg.focus_mode {
                ui.label(RichText::new("专注").size(12.0).color(theme.accent));
            }
            if app.cfg.typewriter {
                ui.label(RichText::new("打字机").size(12.0).color(theme.accent));
            }
        });
    });
}

// ===========================================================================
// Built-in file browser
// ===========================================================================

/// Height of one row of the listing.
const PICK_ROW_H: f32 = 22.0;

/// The built-in browser, drawn as a window.
///
/// This exists because the OS panel cannot be trusted to let the user pick a
/// `.md` file (see `picker.rs`). It reads directories itself, filters to
/// Markdown by default, and is fully keyboard driven: `↑`/`↓` move, `Enter`
/// opens, `Esc` closes.
pub fn file_picker(ctx: &egui::Context, app: &mut App) {
    if !app.picker.open {
        return;
    }
    app.picker.tick();

    let theme = app.theme.clone();
    let vis = app.picker.visible();
    let total = vis.len();
    let folder_mode = app.picker.picking_folder;

    let path_id = egui::Id::new("rustmd-picker-path");
    let filter_id = egui::Id::new("rustmd-picker-filter");

    // Deferred actions: the widgets below only set these, and they are applied
    // after the window has been drawn so there is never a borrow conflict
    // between the UI and `App`.
    let mut close = false;
    let mut open_sel = false;
    let mut accept_folder = false;
    let mut system_dialog = false;
    let mut go_up = false;
    let mut go_home = false;
    let mut do_refresh = false;
    let mut enter_dir: Option<PathBuf> = None;
    let mut show_all: Option<bool> = None;

    let mut open_flag = true;
    egui::Window::new(if folder_mode {
        "选择文件夹"
    } else {
        "打开 Markdown"
    })
    .open(&mut open_flag)
    .collapsible(false)
    .resizable(true)
    .default_size([660.0, 470.0])
    .min_width(420.0)
    .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
    .show(ctx, |ui| {
        // ------------------------------------------------------- location bar
        ui.horizontal(|ui| {
            if ui.small_button("↑").on_hover_text("上一级").clicked() {
                go_up = true;
            }
            if ui.small_button("🏠").on_hover_text("主目录").clicked() {
                go_home = true;
            }
            if ui.small_button("⟳").on_hover_text("重新读取").clicked() {
                do_refresh = true;
            }
            let w = (ui.available_width() - 6.0).max(140.0);
            let resp = ui.add_sized(
                [w, 22.0],
                TextEdit::singleline(&mut app.picker.path_buf)
                    .id(path_id)
                    .hint_text("输入路径后回车"),
            );
            if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let typed = PathBuf::from(app.picker.path_buf.trim());
                if typed.is_dir() {
                    enter_dir = Some(typed);
                } else if typed.is_file() {
                    app.picker.go(
                        typed
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| app.picker.dir.clone()),
                    );
                    enter_dir = Some(typed);
                } else {
                    app.picker.refresh();
                }
            }
        });

        // ------------------------------------------------------------ filters
        ui.horizontal(|ui| {
            let mut all = app.picker.show_all;
            if ui
                .checkbox(&mut all, RichText::new("显示所有文件").size(12.0))
                .changed()
            {
                show_all = Some(all);
            }
            ui.add_space(8.0);
            ui.label(RichText::new("筛选").size(12.0).color(theme.text_muted));
            ui.add_sized(
                [150.0, 20.0],
                TextEdit::singleline(&mut app.picker.filter)
                    .id(filter_id)
                    .hint_text("文件名包含…"),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let text = if app.picker.listing.truncated {
                    format!("{total} 项（已截断）")
                } else {
                    format!("{total} 项")
                };
                ui.label(RichText::new(text).size(11.5).color(theme.text_faint));
            });
        });

        if let Some(err) = app.picker.listing.error.clone() {
            ui.label(
                RichText::new(format!("无法读取目录：{err}"))
                    .size(12.0)
                    .color(theme.accent),
            );
        }

        ui.add_space(4.0);

        // ------------------------------------------------------------ listing
        let list_h = (ui.available_height() - 46.0).clamp(120.0, 640.0);
        egui::ScrollArea::vertical()
            .id_salt("rustmd-picker-list")
            .auto_shrink([false, false])
            .max_height(list_h)
            .show_rows(ui, PICK_ROW_H, total, |ui, range| {
                for vi in range {
                    let Some(&idx) = vis.get(vi) else { continue };
                    // Copy just what the row needs, so `app` is free to be
                    // mutated below.
                    let (name, is_dir, size, modified, path) = {
                        let e = &app.picker.listing.entries[idx];
                        (e.name.clone(), e.is_dir, e.size, e.modified, e.path.clone())
                    };
                    let selected = vi == app.picker.cursor;
                    let width = ui.available_width();
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(width, PICK_ROW_H), Sense::click());
                    if selected || resp.hovered() {
                        let bg = if selected { theme.active } else { theme.hover };
                        ui.painter().rect_filled(rect, 4.0, bg);
                    }
                    let name_color = if is_dir { theme.text_muted } else { theme.text };
                    ui.painter().text(
                        rect.left_center() + vec2(8.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        elide(&name, 48),
                        FontId::new(13.0, fonts::family_for(false, false)),
                        name_color,
                    );
                    if is_dir {
                        ui.painter().text(
                            rect.right_center() - vec2(8.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            "文件夹",
                            FontId::new(11.0, fonts::family_for(false, false)),
                            theme.text_faint,
                        );
                    } else {
                        let meta = match modified {
                            Some(t) => format!("{}  {}", picker::human_size(size), picker::human_time(t)),
                            None => picker::human_size(size),
                        };
                        ui.painter().text(
                            rect.right_center() - vec2(8.0, 0.0),
                            egui::Align2::RIGHT_CENTER,
                            meta,
                            FontId::new(11.0, font_family_mono()),
                            theme.text_faint,
                        );
                    }
                    if resp.clicked() {
                        app.picker.cursor = vi;
                    }
                    if resp.double_clicked() {
                        if is_dir {
                            enter_dir = Some(path);
                        } else {
                            open_sel = true;
                        }
                    }
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                }
            });

        // ----------------------------------------------------------- keyboard
        let editing = ui.memory(|m| m.has_focus(path_id) || m.has_focus(filter_id));
        if !editing && total > 0 {
            if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                app.picker.cursor = (app.picker.cursor + 1).min(total - 1);
            }
            if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                app.picker.cursor = app.picker.cursor.saturating_sub(1);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                open_sel = true;
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            close = true;
        }

        // -------------------------------------------------------------- footer
        ui.add_space(6.0);
        ui.separator();
        ui.horizontal(|ui| {
            let hint = app
                .picker
                .selected()
                .map(|e| e.path.display().to_string())
                .unwrap_or_else(|| "未选中".to_string());
            ui.label(RichText::new(elide(&hint, 54)).size(11.5).color(theme.text_faint));
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let primary = if folder_mode { "选择此文件夹" } else { "打开" };
                let btn = egui::Button::new(RichText::new(primary).color(Color32::WHITE))
                    .fill(theme.accent);
                if ui.add_enabled(total > 0, btn).clicked() {
                    if folder_mode {
                        accept_folder = true;
                    } else {
                        open_sel = true;
                    }
                }
                if ui.button("取消").clicked() {
                    close = true;
                }
                if !folder_mode && ui.button("系统对话框…").clicked() {
                    system_dialog = true;
                }
            });
        });
    });

    if !open_flag {
        close = true;
    }

    // ------------------------------------------------------------ apply them
    if let Some(p) = enter_dir {
        if p.is_dir() {
            app.picker.go(p);
        } else {
            app.picker.close();
            app.open_path(&p);
        }
        return;
    }
    if let Some(all) = show_all {
        app.picker.set_show_all(all);
    }
    if go_up {
        app.picker.go_up();
    }
    if go_home {
        app.picker.go_home();
    }
    if do_refresh {
        app.picker.refresh();
    }
    if open_sel {
        app.accept_picker_file();
    } else if accept_folder {
        app.accept_picker_folder();
    }
    if system_dialog {
        app.picker.close();
        app.open_system_dialog();
    }
    if close {
        app.picker.close();
    }
}

/// Shorten `s` to at most `max` characters, ending with an ellipsis.
fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A full-window hint while files are being dragged over the app.
///
/// Dropping a `.md` file onto the window is the quickest way to open one, and
/// without this the window looks inert while the pointer is over it.
pub fn drop_overlay(ctx: &egui::Context, app: &App) {
    let Some(name) = app.drag_hint.as_ref() else {
        return;
    };
    let screen = ctx.screen_rect();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::new("rustmd-drop"));
    let p = ctx.layer_painter(layer);
    p.rect_filled(screen, 0.0, Color32::from_black_alpha(80));
    p.rect_stroke(
        screen.shrink(10.0),
        12.0,
        egui::Stroke::new(2.0_f32, app.theme.accent),
        egui::StrokeKind::Inside,
    );
    p.text(
        screen.center() - vec2(0.0, 12.0),
        egui::Align2::CENTER_CENTER,
        "松开以打开",
        FontId::new(17.0, fonts::family_for(false, false)),
        Color32::WHITE,
    );
    p.text(
        screen.center() + vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        elide(name, 60),
        FontId::new(15.0, fonts::family_for(true, false)),
        Color32::WHITE,
    );
}

/// The monospace face, through the same accessor the rest of the UI uses.
fn font_family_mono() -> egui::FontFamily {
    fonts::mono_family_for(false)
}

// ===========================================================================
// Dialogs
// ===========================================================================

pub fn dialogs(ctx: &egui::Context, app: &mut App) {
    file_picker(ctx, app);

    if app.show_help {
        let mut open = true;
        egui::Window::new("快捷键")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let rows = [
                    ("⌘N / ⌘O / ⌘S", "新建 / 打开 / 保存"),
                    ("⇧⌘S", "另存为"),
                    ("⌘F", "查找与替换"),
                    ("⌘Z / ⇧⌘Z", "撤销 / 重做"),
                    ("⌘B / ⌘I / ⌘E", "加粗 / 斜体 / 行内代码"),
                    ("⌘R", "切换阅读模式"),
                    ("⌘⇧E", "循环切换 实时预览 / 源码 / 分栏"),
                    ("⌘/", "切换浅色深色主题"),
                    ("⌘K", "插入链接"),
                    ("⌘1…⌘6", "设为 1~6 级标题"),
                    ("⌘0", "设为正文"),
                    ("Tab / ⇧Tab", "增加 / 减少缩进"),
                    ("⇧Enter", "段内强制换行"),
                    ("↑ / ↓", "在块之间移动光标"),
                    ("Esc", "退出当前块"),
                    ("⌘ + 点击", "打开链接"),
                ];
                egui::Grid::new("shortcuts")
                    .num_columns(2)
                    .spacing([26.0, 6.0])
                    .show(ui, |ui| {
                        for (k, v) in rows {
                            ui.label(RichText::new(k).monospace().size(12.5));
                            ui.label(RichText::new(v).size(12.5));
                            ui.end_row();
                        }
                    });
            });
        app.show_help = open;
    }

    if app.show_about {
        let mut open = true;
        egui::Window::new("关于 rustmd")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("rustmd")
                        .font(FontId::new(20.0, fonts::family_for(true, false))),
                );
                ui.label(
                    RichText::new("用 Rust 写的 Markdown 编辑器 / 阅读器")
                        .size(12.5)
                        .color(app.theme.text_muted),
                );
                ui.add_space(8.0);
                ui.label(RichText::new(format!("版本 {}", env!("CARGO_PKG_VERSION"))).size(12.0));
                ui.add_space(6.0);
                ui.label(RichText::new("当前字体").size(12.0).color(app.theme.text_muted));
                ui.label(RichText::new(fonts::describe(&app.font_report)).size(12.0));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "已解析 {} 个字体面",
                        app.font_report.resolved.len()
                    ))
                    .size(11.5)
                    .color(app.theme.text_faint),
                );
            });
        app.show_about = open;
    }

    if let Some(prompt) = app.prompt.clone() {
        let mut open = true;
        egui::Window::new(&prompt.title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(RichText::new(&prompt.body).size(13.0));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let labels: Vec<String> = prompt.choices.iter().map(|c| c.0.clone()).collect();
                    for (i, label) in labels.iter().enumerate() {
                        let primary = i == 0;
                        let btn = if primary {
                            egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                .fill(app.theme.accent)
                        } else {
                            egui::Button::new(RichText::new(label))
                        };
                        if ui.add(btn).clicked() {
                            let choice = prompt.choices[i].1;
                            app.resolve_prompt(choice);
                        }
                    }
                });
            });
        if !open {
            app.prompt = None;
        }
    }
}
