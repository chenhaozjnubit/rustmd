//! Colour palettes. Two hand-tuned themes that follow Typora's visual language:
//! a roomy, low-contrast reading surface with a distinct accent for links and
//! a muted treatment for Markdown markers.

use egui::Color32;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    pub fn is_dark(self) -> bool {
        matches!(self, ThemeMode::Dark)
    }
    pub fn toggled(self) -> Self {
        match self {
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
        }
    }
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            ThemeMode::Light => "浅色",
            ThemeMode::Dark => "深色",
        }
    }
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb(
        ((hex >> 16) & 0xFF) as u8,
        ((hex >> 8) & 0xFF) as u8,
        (hex & 0xFF) as u8,
    )
}

/// The full palette. Not every entry is used by every view yet, but keeping
/// the set complete is what makes the two themes feel coherent.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Theme {
    pub mode: ThemeMode,

    /// The page the document is written on.
    pub bg: Color32,
    /// Side panels (file tree / outline).
    pub sidebar: Color32,
    /// Toolbar and status bar.
    pub chrome: Color32,

    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub heading: Color32,

    pub accent: Color32,
    pub link: Color32,
    pub selection: Color32,

    pub border: Color32,
    pub border_strong: Color32,
    pub hover: Color32,
    pub active: Color32,

    pub code_bg: Color32,
    pub code_text: Color32,
    pub inline_code_bg: Color32,
    pub inline_code_text: Color32,

    pub quote_bar: Color32,
    pub quote_bg: Color32,
    pub quote_text: Color32,

    pub marker: Color32,
    pub marker_dim: Color32,

    pub highlight_bg: Color32,
    pub highlight_text: Color32,

    pub table_head_bg: Color32,
    pub table_stripe: Color32,

    pub line_hl: Color32,
    pub focus_dim: Color32,
    pub scrollbar: Color32,

    pub danger: Color32,
    pub ok: Color32,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            mode: ThemeMode::Light,
            bg: rgb(0xFFFFFF),
            sidebar: rgb(0xF7F8FA),
            chrome: rgb(0xFBFBFC),
            text: rgb(0x262B33),
            text_muted: rgb(0x6E7681),
            text_faint: rgb(0xAEB6C0),
            heading: rgb(0x15181D),
            accent: rgb(0x2F6FEB),
            link: rgb(0x1F6FEB),
            selection: rgb(0xCFE3FF),
            border: rgb(0xE4E8ED),
            border_strong: rgb(0xCDD4DC),
            hover: rgb(0xEEF1F5),
            active: rgb(0xE1E7EF),
            code_bg: rgb(0xF6F8FA),
            code_text: rgb(0x24292F),
            inline_code_bg: rgb(0xEFF1F4),
            inline_code_text: rgb(0xB4436A),
            quote_bar: rgb(0xD3D9E0),
            quote_bg: rgb(0xFAFBFC),
            quote_text: rgb(0x5A6472),
            marker: rgb(0xBFC7D1),
            marker_dim: rgb(0xD6DCE3),
            highlight_bg: rgb(0xFFF3B0),
            highlight_text: rgb(0x4A3B00),
            table_head_bg: rgb(0xF4F6F8),
            table_stripe: rgb(0xFAFBFC),
            line_hl: rgb(0xF2F6FD),
            focus_dim: rgb(0xD8DDE4),
            scrollbar: rgb(0xC8CFD8),
            danger: rgb(0xD1242F),
            ok: rgb(0x1A7F37),
        }
    }

    pub fn dark() -> Self {
        Self {
            mode: ThemeMode::Dark,
            bg: rgb(0x1C1F25),
            sidebar: rgb(0x16191E),
            chrome: rgb(0x191C22),
            text: rgb(0xD5DBE3),
            text_muted: rgb(0x8B94A1),
            text_faint: rgb(0x5C646F),
            heading: rgb(0xF0F3F7),
            accent: rgb(0x6CB6FF),
            link: rgb(0x6CB6FF),
            selection: rgb(0x2C4A73),
            border: rgb(0x2B3038),
            border_strong: rgb(0x3A414B),
            hover: rgb(0x252A31),
            active: rgb(0x2E343D),
            code_bg: rgb(0x14171B),
            code_text: rgb(0xC9D1D9),
            inline_code_bg: rgb(0x2A2F37),
            inline_code_text: rgb(0xE8A0BF),
            quote_bar: rgb(0x3B434D),
            quote_bg: rgb(0x21252B),
            quote_text: rgb(0x9AA4B0),
            marker: rgb(0x555E6A),
            marker_dim: rgb(0x40474F),
            highlight_bg: rgb(0x5A4A14),
            highlight_text: rgb(0xFFE9A0),
            table_head_bg: rgb(0x242931),
            table_stripe: rgb(0x1F2329),
            line_hl: rgb(0x232A34),
            focus_dim: rgb(0x4A525C),
            scrollbar: rgb(0x3A414B),
            danger: rgb(0xF8716F),
            ok: rgb(0x56D364),
        }
    }

    pub fn for_mode(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Light => Self::light(),
            ThemeMode::Dark => Self::dark(),
        }
    }

    /// Syntax-highlighting theme name to ask `syntect` for.
    pub fn code_theme(&self) -> &'static str {
        match self.mode {
            ThemeMode::Light => "InspiredGitHub",
            ThemeMode::Dark => "base16-ocean.dark",
        }
    }

    /// Apply the palette to the global egui style.
    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.mode.is_dark() {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        visuals.panel_fill = self.chrome;
        visuals.window_fill = self.chrome;
        visuals.window_stroke = egui::Stroke::new(1.0_f32, self.border);
        visuals.extreme_bg_color = self.bg;
        visuals.faint_bg_color = self.sidebar;
        visuals.selection.bg_fill = self.selection;
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, self.accent);
        visuals.hyperlink_color = self.link;
        visuals.override_text_color = Some(self.text);
        visuals.window_corner_radius = egui::CornerRadius::same(8);
        visuals.menu_corner_radius = egui::CornerRadius::same(8);
        visuals.popup_shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if self.mode.is_dark() { 120 } else { 40 }),
        };
        visuals.window_shadow = visuals.popup_shadow;

        visuals.widgets.noninteractive.bg_fill = self.chrome;
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, self.border);
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, self.text_muted);

        visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
        visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, self.text_muted);

        visuals.widgets.hovered.bg_fill = self.hover;
        visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0_f32, self.text);
        visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(5);

        visuals.widgets.active.bg_fill = self.active;
        visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
        visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0_f32, self.text);

        visuals.widgets.open.bg_fill = self.active;
        visuals.widgets.open.bg_stroke = egui::Stroke::NONE;

        ctx.set_visuals(visuals);
    }
}
