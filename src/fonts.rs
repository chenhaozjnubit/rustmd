//! System font loading.
//!
//! Nothing is bundled: the editor resolves the best faces available on the
//! machine so that Latin text gets real weights, Chinese text gets a proper
//! sans with a matching bold, and formulas get an OpenType math font.
//!
//! `.ttc` collections hold several weights, so a face is identified by
//! `(path, index)`. The indices below were read out of the actual name tables of
//! the macOS system fonts; every slot still has an ordered fallback chain, so a
//! missing file degrades instead of failing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{FontData, FontDefinitions, FontFamily};

#[derive(Clone)]
struct Face {
    path: PathBuf,
    index: u32,
}

impl Face {
    fn new(path: impl Into<PathBuf>, index: u32) -> Self {
        Self {
            path: path.into(),
            index,
        }
    }
}

/// Depth-limited filename search, used to find font assets that macOS keeps
/// under a content-addressed directory.
fn find_under(root: &Path, file: &str, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut subdirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
        } else if p.file_name().map(|n| n == file).unwrap_or(false) {
            return Some(p);
        }
    }
    for d in subdirs {
        if let Some(f) = find_under(&d, file, depth - 1) {
            return Some(f);
        }
    }
    None
}

/// Locate a system font asset by filename, checking the plain system font
/// directories first and then the `AssetsV2` font asset store.
fn find_system_font(file: &str, plain_dirs: &[&str]) -> Option<PathBuf> {
    for dir in plain_dirs {
        let p = Path::new(dir).join(file);
        if p.exists() {
            return Some(p);
        }
    }
    let assets = Path::new("/System/Library/AssetsV2");
    if assets.is_dir() {
        if let Ok(entries) = std::fs::read_dir(assets) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("com_apple_MobileAsset_Font") {
                    if let Some(f) = find_under(&e.path(), file, 4) {
                        return Some(f);
                    }
                }
            }
        }
    }
    None
}

const FONT_DIRS: &[&str] = &[
    "/System/Library/Fonts",
    "/System/Library/Fonts/Supplemental",
    "/Library/Fonts",
];

const SANS_FILE: &str = "HelveticaNeue.ttc";
const SANS_FALLBACKS: &[&str] = &["Helvetica.ttc", "Arial.ttf", "Geneva.ttf"];
const MONO_FILE: &str = "Menlo.ttc";
const MONO_FALLBACKS: &[&str] = &["Monaco.ttf", "SFNSMono.ttf", "Courier.ttc"];
const CJK_FILE: &str = "PingFang.ttc";
const CJK_FALLBACKS: &[&str] = &["Hiragino Sans GB.ttc", "STHeiti Medium.ttc", "Arial Unicode.ttf"];
const SERIF_FILE: &str = "STIXTwoText.ttf";
const SERIF_FALLBACKS: &[&str] = &["Georgia.ttf", "Times New Roman.ttf"];
const MATH_FILE: &str = "STIXTwoMath.otf";
const MATH_FALLBACKS: &[&str] = &["STIXGeneral.otf", "Apple Symbols.ttf"];
const MATH_ITALIC_FILE: &str = "STIXTwoText-Italic.ttf";
const MATH_ITALIC_FALLBACKS: &[&str] = &["STIXGeneralItalic.otf"];

/// Which family a CJK collection ended up being, since face indices differ.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CjkKind {
    PingFang,
    Hiragino,
    Other,
}

/// Face indices per weight for each supported collection.
fn sans_faces(path: &Path) -> (u32, u32, u32, u32) {
    // Helvetica Neue: Regular, Bold, Italic, Bold Italic
    // Helvetica:      same layout
    let _ = path;
    (0, 1, 2, 3)
}

fn mono_faces(path: &Path) -> (u32, u32) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    match name.as_deref() {
        Some("Menlo.ttc") => (0, 1),
        // Monaco and Courier ship a single face; reuse it for bold.
        _ => (0, 0),
    }
}

fn cjk_faces(kind: CjkKind) -> (u32, u32) {
    match kind {
        // 苹方-简 常规体 / 中粗体
        CjkKind::PingFang => (3, 11),
        // 冬青黑体简体中文 W3 / W6
        CjkKind::Hiragino => (0, 2),
        CjkKind::Other => (0, 0),
    }
}

#[derive(Debug, Clone, Default)]
pub struct FontReport {
    pub resolved: Vec<(&'static str, String)>,
    pub missing: Vec<&'static str>,
}

impl FontReport {
    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        let mut s = String::new();
        for (k, p) in &self.resolved {
            s.push_str(&format!("  {k:<16} {p}\n"));
        }
        for k in &self.missing {
            s.push_str(&format!("  {k:<16} (missing, using built-in)\n"));
        }
        s
    }

}

fn read_face(face: &Face) -> Option<Vec<u8>> {
    let bytes = std::fs::read(&face.path).ok()?;
    if bytes.len() < 64 {
        return None;
    }
    Some(bytes)
}

struct Builder {
    defs: FontDefinitions,
    report: FontReport,
}

impl Builder {
    fn add(&mut self, key: &'static str, face: &Face) -> bool {
        let Some(bytes) = read_face(face) else {
            return false;
        };
        let mut data = FontData::from_owned(bytes);
        data.index = face.index;
        self.defs.font_data.insert(key.to_owned(), Arc::new(data));
        self.report
            .resolved
            .push((key, format!("{}#{}", face.path.display(), face.index)));
        true
    }
}

/// Install the full font stack into the egui context.
pub fn install(ctx: &egui::Context) -> FontReport {
    let mut b = Builder {
        defs: FontDefinitions::default(),
        report: FontReport::default(),
    };

    // ---- Latin sans, four weights -----------------------------------------
    let sans_path = find_system_font(SANS_FILE, FONT_DIRS)
        .or_else(|| SANS_FALLBACKS.iter().find_map(|f| find_system_font(f, FONT_DIRS)));
    let mut sans_keys: Vec<&'static str> = Vec::new();
    let mut bold_keys: Vec<&'static str> = Vec::new();
    let mut italic_keys: Vec<&'static str> = Vec::new();
    let mut bold_italic_keys: Vec<&'static str> = Vec::new();
    if let Some(p) = sans_path {
        let (r, bo, it, bi) = sans_faces(&p);
        if b.add("sans", &Face::new(p.clone(), r)) {
            sans_keys.push("sans");
        }
        if b.add("sans-bold", &Face::new(p.clone(), bo)) {
            bold_keys.push("sans-bold");
        }
        if b.add("sans-italic", &Face::new(p.clone(), it)) {
            italic_keys.push("sans-italic");
        }
        if b.add("sans-bold-italic", &Face::new(p.clone(), bi)) {
            bold_italic_keys.push("sans-bold-italic");
        }
    } else {
        b.report.missing.push("sans");
    }

    // ---- monospace ---------------------------------------------------------
    let mono_path = find_system_font(MONO_FILE, FONT_DIRS)
        .or_else(|| MONO_FALLBACKS.iter().find_map(|f| find_system_font(f, FONT_DIRS)));
    let mut mono_keys: Vec<&'static str> = Vec::new();
    let mut mono_bold_keys: Vec<&'static str> = Vec::new();
    if let Some(p) = mono_path {
        let (r, bo) = mono_faces(&p);
        if b.add("mono", &Face::new(p.clone(), r)) {
            mono_keys.push("mono");
        }
        if bo != r {
            if b.add("mono-bold", &Face::new(p.clone(), bo)) {
                mono_bold_keys.push("mono-bold");
            }
        }
    } else {
        b.report.missing.push("mono");
    }

    // ---- CJK ---------------------------------------------------------------
    let mut cjk_regular: Vec<&'static str> = Vec::new();
    let mut cjk_bold: Vec<&'static str> = Vec::new();
    let cjk_found = find_system_font(CJK_FILE, FONT_DIRS)
        .map(|p| (p, CjkKind::PingFang))
        .or_else(|| {
            CJK_FALLBACKS.iter().find_map(|f| {
                find_system_font(f, FONT_DIRS).map(|p| {
                    let kind = if *f == "Hiragino Sans GB.ttc" {
                        CjkKind::Hiragino
                    } else {
                        CjkKind::Other
                    };
                    (p, kind)
                })
            })
        });
    if let Some((p, kind)) = cjk_found {
        let (r, bo) = cjk_faces(kind);
        if b.add("cjk", &Face::new(p.clone(), r)) {
            cjk_regular.push("cjk");
        }
        if bo != r && b.add("cjk-bold", &Face::new(p.clone(), bo)) {
            cjk_bold.push("cjk-bold");
        }
    } else {
        b.report.missing.push("cjk");
    }

    // ---- serif (used for math text and the serif reading option) -----------
    let mut serif_keys: Vec<&'static str> = Vec::new();
    if let Some(p) = find_system_font(SERIF_FILE, FONT_DIRS)
        .or_else(|| SERIF_FALLBACKS.iter().find_map(|f| find_system_font(f, FONT_DIRS)))
    {
        if b.add("serif", &Face::new(p, 0)) {
            serif_keys.push("serif");
        }
    } else {
        b.report.missing.push("serif");
    }

    // ---- math --------------------------------------------------------------
    let mut math_keys: Vec<&'static str> = Vec::new();
    if let Some(p) = find_system_font(MATH_FILE, FONT_DIRS)
        .or_else(|| MATH_FALLBACKS.iter().find_map(|f| find_system_font(f, FONT_DIRS)))
    {
        if b.add("math", &Face::new(p, 0)) {
            math_keys.push("math");
        }
    } else {
        b.report.missing.push("math");
    }
    let mut math_italic_keys: Vec<&'static str> = Vec::new();
    if let Some(p) = find_system_font(MATH_ITALIC_FILE, FONT_DIRS)
        .or_else(|| MATH_ITALIC_FALLBACKS.iter().find_map(|f| find_system_font(f, FONT_DIRS)))
    {
        if b.add("math-italic", &Face::new(p, 0)) {
            math_italic_keys.push("math-italic");
        }
    }

    // ---- build the fallback chains -----------------------------------------
    const EMOJI: &[&str] = &["NotoEmoji-Regular", "emoji-icon-font"];

    let chain = |parts: Vec<Vec<&'static str>>| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for seg in parts {
            for k in seg {
                let s = k.to_string();
                if !v.contains(&s) {
                    v.push(s);
                }
            }
        }
        // egui's built-in fonts are the last resort for anything still missing.
        for e in EMOJI {
            if !v.contains(&e.to_string()) {
                v.push((*e).to_string());
            }
        }
        if !v.contains(&"Ubuntu-Light".to_string()) {
            v.push("Ubuntu-Light".into());
        }
        v
    };

    let cjk_ref = || cjk_regular.clone();
    let cjk_bold_ref = || cjk_bold.clone();

    let proportional = chain(vec![
        sans_keys.clone(),
        cjk_ref(),
        mono_keys.clone(),
    ]);
    let monospace = chain(vec![mono_keys.clone(), cjk_ref(), sans_keys.clone()]);
    let bold = chain(vec![
        bold_keys.clone(),
        cjk_bold_ref(),
        cjk_ref(),
        sans_keys.clone(),
    ]);
    let italic = chain(vec![
        italic_keys.clone(),
        sans_keys.clone(),
        cjk_ref(),
    ]);
    let bold_italic = chain(vec![
        bold_italic_keys.clone(),
        bold_keys.clone(),
        italic_keys.clone(),
        sans_keys.clone(),
        cjk_bold_ref(),
    ]);
    let mono_bold = chain(vec![
        mono_bold_keys.clone(),
        mono_keys.clone(),
        cjk_bold_ref(),
    ]);
    let serif = chain(vec![serif_keys.clone(), cjk_ref(), sans_keys.clone()]);
    let math = chain(vec![
        math_keys.clone(),
        serif_keys.clone(),
        sans_keys.clone(),
        cjk_ref(),
    ]);
    let math_italic = chain(vec![
        math_italic_keys.clone(),
        math_keys.clone(),
        serif_keys.clone(),
        sans_keys.clone(),
    ]);

    let mut families: Vec<(String, Vec<String>)> = vec![
        ("bold".into(), bold),
        ("italic".into(), italic),
        ("bold-italic".into(), bold_italic),
        ("mono-bold".into(), mono_bold),
        ("serif".into(), serif),
        ("math".into(), math),
        ("mathit".into(), math_italic),
    ];
    b.defs
        .families
        .insert(FontFamily::Proportional, proportional);
    b.defs.families.insert(FontFamily::Monospace, monospace);
    for (name, list) in families.drain(..) {
        b.defs
            .families
            .insert(FontFamily::Name(name.into()), list);
    }

    ctx.set_fonts(b.defs);
    b.report
}

/// Resolve a `FontFamily` for a combination of weight and slant.
pub fn family_for(bold: bool, italic: bool) -> FontFamily {
    match (bold, italic) {
        (true, true) => FontFamily::Name("bold-italic".into()),
        (true, false) => FontFamily::Name("bold".into()),
        (false, true) => FontFamily::Name("italic".into()),
        (false, false) => FontFamily::Proportional,
    }
}

pub fn mono_family_for(bold: bool) -> FontFamily {
    if bold {
        FontFamily::Name("mono-bold".into())
    } else {
        FontFamily::Monospace
    }
}

#[allow(dead_code)]
pub fn serif_family() -> FontFamily {
    FontFamily::Name("serif".into())
}

#[allow(dead_code)]
pub fn math_family() -> FontFamily {
    FontFamily::Name("math".into())
}

#[allow(dead_code)]
pub fn math_italic_family() -> FontFamily {
    FontFamily::Name("mathit".into())
}

/// Human-readable name of the face actually in use, for the About/status UI.
pub fn describe(report: &FontReport) -> String {
    let pick = |key: &str| -> String {
        report
            .resolved
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, p)| {
                let path = Path::new(p.split('#').next().unwrap_or(p));
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.clone())
            })
            .unwrap_or_else(|| "内置".into())
    };
    format!("正文 {} · 中文 {} · 等宽 {}", pick("sans"), pick("cjk"), pick("mono"))
}
