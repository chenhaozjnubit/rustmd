//! Persistent user settings, stored as JSON next to the platform's app data.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::ThemeMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: ThemeMode,
    /// Body font size in points.
    pub font_size: f32,
    /// Width of the centred text column, in points.
    pub content_width: f32,
    /// Paragraph line spacing multiplier.
    pub line_height: f32,
    pub show_sidebar: bool,
    pub sidebar_width: f32,
    pub show_outline: bool,
    pub outline_width: f32,
    /// Dim every block except the one being edited.
    pub focus_mode: bool,
    /// Keep the caret vertically centred while typing.
    pub typewriter: bool,
    /// Show raw Markdown markers around the caret's inline span.
    pub show_markers: bool,
    /// Automatically save the file this many seconds after the last edit.
    /// `0` disables auto-save.
    pub autosave_secs: u64,
    pub wrap_code: bool,
    pub recent: Vec<PathBuf>,
    pub last_dir: Option<PathBuf>,
    /// Remembered window geometry.
    pub window: Option<(f32, f32, f32, f32)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: ThemeMode::Light,
            font_size: 16.5,
            content_width: 780.0,
            line_height: 1.72,
            show_sidebar: true,
            sidebar_width: 244.0,
            show_outline: true,
            outline_width: 236.0,
            focus_mode: false,
            typewriter: false,
            show_markers: true,
            autosave_secs: 0,
            wrap_code: false,
            recent: Vec::new(),
            last_dir: None,
            window: None,
        }
    }
}

pub fn config_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home)
                .join("Library/Application Support/rustmd/config.json");
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("rustmd/config.json");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/rustmd/config.json");
    }
    PathBuf::from("rustmd-config.json")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = config_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, json);
        }
    }

    pub fn remember(&mut self, path: &Path) {
        let p = path.to_path_buf();
        self.recent.retain(|x| x != &p);
        self.recent.insert(0, p);
        self.recent.truncate(12);
        if let Some(dir) = path.parent() {
            self.last_dir = Some(dir.to_path_buf());
        }
    }

    pub fn drop_missing_recent(&mut self) {
        self.recent.retain(|p| p.exists());
    }
}
