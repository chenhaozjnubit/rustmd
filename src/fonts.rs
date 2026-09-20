//! System font loading.
//!
//! Nothing is bundled: the editor resolves the best faces available on the
//! machine so that Latin text gets real weights, Chinese text gets a proper
//! sans with a matching bold, and formulas get a real math font.
//!
//! The same face is spelled differently per platform. macOS ships weight
//! *collections* — `HelveticaNeue.ttc` holds regular, bold, italic and bold
//! italic as faces 0..3 — while Windows ships one file per weight
//! (`segoeui.ttf`, `segoeuib.ttf`, …). So the tables below name the families
//! they want and the face is located by reading the file's `name` table rather
//! than by trusting an index. Windows' own Chinese families only exist as
//! two-face collections (`msyh.ttc` is Microsoft YaHei plus Microsoft YaHei
//! UI, `simsun.ttc` is SimSun plus NSimSun) and their order is not a documented
//! contract; guessing wrong there is a screenful of tofu boxes, and the picker
//! is drawn with the same fonts, so the app looks broken rather than
//! mis-configured.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use egui::{FontData, FontDefinitions, FontFamily};

// ===========================================================================
// Just enough SFNT to ask a face what it is called
// ===========================================================================

/// Reading a face's `name` table. A `.ttc` is a header plus a table of face
/// offsets; a plain `.ttf`/`.otf` is a single face at offset zero. Everything
/// below is bounds-checked and simply gives up on anything unexpected, because
/// a font file that cannot be understood must degrade to the next candidate
/// rather than take the process down.
mod sfnt {
    fn be16(d: &[u8], o: usize) -> Option<u16> {
        Some(u16::from_be_bytes([*d.get(o)?, *d.get(o + 1)?]))
    }

    fn be32(d: &[u8], o: usize) -> Option<u32> {
        Some(u32::from_be_bytes([
            *d.get(o)?,
            *d.get(o + 1)?,
            *d.get(o + 2)?,
            *d.get(o + 3)?,
        ]))
    }

    /// Byte offset of every face in the file. Empty when this is not a font.
    pub fn face_offsets(d: &[u8]) -> Vec<usize> {
        let Some(tag) = be32(d, 0) else {
            return Vec::new();
        };
        if tag == u32::from_be_bytes(*b"ttcf") {
            let count = be32(d, 8).unwrap_or(0) as usize;
            let mut v = Vec::new();
            for i in 0..count.min(64) {
                match be32(d, 12 + i * 4) {
                    Some(o) if (o as usize) < d.len() => v.push(o as usize),
                    _ => break,
                }
            }
            v
        } else if matches!(tag, 0x0001_0000 | 0x4F54_544F | 0x7472_7565) {
            // TrueType, OpenType/CFF, and the old Macintosh 'true' tag.
            vec![0]
        } else {
            Vec::new()
        }
    }

    /// The names a face answers to, as far as this module cares.
    #[derive(Debug, Default, Clone)]
    pub struct Names {
        pub family: Option<String>,
        pub subfamily: Option<String>,
        /// nameID 16: the family with the weight left out, e.g. "Helvetica
        /// Neue" for "Helvetica Neue Condensed Bold". Absent in fonts that do
        /// not need it.
        pub typographic_family: Option<String>,
    }

    /// Read the `name` table of the face at `base`.
    pub fn names(d: &[u8], base: usize) -> Option<Names> {
        let table = name_table(d, base)?;
        Some(Names {
            family: string(d, table, 1),
            subfamily: string(d, table, 2),
            typographic_family: string(d, table, 16),
        })
    }

    fn name_table(d: &[u8], base: usize) -> Option<usize> {
        let count = be16(d, base + 4)? as usize;
        for i in 0..count.min(512) {
            let record = base + 12 + i * 16;
            if d.get(record..record + 4)? == b"name" {
                return Some(be32(d, record + 8)? as usize);
            }
        }
        None
    }

    /// The `want` name of this face, preferring the Windows/English record so
    /// that a localised name table cannot make the same font unrecognisable
    /// from one machine to the next.
    fn string(d: &[u8], table: usize, want: u16) -> Option<String> {
        let count = be16(d, table + 2)? as usize;
        let pool = table + be16(d, table + 4)? as usize;
        let mut best: Option<(u8, String)> = None;
        for i in 0..count.min(1024) {
            let record = table + 6 + i * 12;
            let (platform, lang, id, len, off) = match (
                be16(d, record),
                be16(d, record + 2),
                be16(d, record + 4),
                be16(d, record + 6),
                be16(d, record + 8),
                be16(d, record + 10),
            ) {
                (Some(p), Some(_encoding), Some(l), Some(i), Some(n), Some(o)) => (p, l, i, n, o),
                _ => continue,
            };
            if id != want {
                continue;
            }
            let start = pool + off as usize;
            let Some(bytes) = d.get(start..start + len as usize) else {
                continue;
            };
            let ranked = match platform {
                // 3 = Windows (UCS-2), 0 = Unicode, 1 = Macintosh (ASCII).
                3 => (if lang == 0x0409 { 0 } else { 2 }, utf16(bytes)),
                0 => (1, utf16(bytes)),
                1 => (3, latin1(bytes)),
                _ => continue,
            };
            let (rank, Some(text)) = ranked else { continue };
            if best.as_ref().map_or(true, |(b, _)| rank < *b) {
                best = Some((rank, text));
            }
        }
        best.map(|(_, s)| s)
    }

    fn utf16(bytes: &[u8]) -> Option<String> {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .take_while(|u| *u != 0)
            .collect();
        String::from_utf16(&units).ok().filter(|s| !s.is_empty())
    }

    fn latin1(bytes: &[u8]) -> Option<String> {
        let s: String = bytes.iter().map(|&b| b as char).collect();
        let s = s.trim_end_matches('\0');
        (!s.is_empty()).then(|| s.to_owned())
    }
}

// ===========================================================================
// Choosing a face
// ===========================================================================

/// Which style we are after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Weight {
    Regular,
    Bold,
    Italic,
    BoldItalic,
}

impl Weight {
    fn wants_bold(self) -> bool {
        matches!(self, Self::Bold | Self::BoldItalic)
    }

    fn wants_italic(self) -> bool {
        matches!(self, Self::Italic | Self::BoldItalic)
    }
}

/// Tokens that mean "heavier than regular" in a font name. The `W6`/`W7`
/// spellings are how Apple's Hiragino faces carry their weight, because their
/// subfamily is plain "Regular"; "Medium" is the weakest accept and only ever
/// wins when nothing heavier exists.
const BOLD_TOKENS: &[&str] = &[
    "bold",
    "black",
    "heavy",
    "semibold",
    "demibold",
    "medium",
    "w6",
    "w7",
    "w8",
    "w9",
];

const ITALIC_TOKENS: &[&str] = &["italic", "oblique"];

/// Widths that are not what "Helvetica Neue" means by default. Still better
/// than nothing, never better than the plain face.
const OFF_WIDTH: &[&str] = &["condensed", "narrow", "compressed", "expanded"];

/// One family as a platform ships it: the files worth trying, and the names
/// the family answers to inside them.
struct Family {
    files: &'static [&'static str],
    names: &'static [&'static str],
}

impl Family {
    const fn new(files: &'static [&'static str], names: &'static [&'static str]) -> Self {
        Self { files, names }
    }
}

/// Lowercase and drop the punctuation font names disagree about, so that
/// "Helvetica Neue", "HelveticaNeue" and "helvetica-neue" all compare equal.
fn squash(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn weight_rank(names: &sfnt::Names, want: Weight) -> Option<u32> {
    let sub = names.subfamily.as_deref().unwrap_or("");
    // A name that spells the weight out has to be read as a whole, because the
    // subfamily of "Hiragino Sans GB W6" is just "Regular", and the subfamily
    // of "Helvetica Neue Condensed Bold" is where the word "Condensed" lives.
    let full = squash(&format!(
        "{} {} {}",
        names.family.as_deref().unwrap_or(""),
        names.typographic_family.as_deref().unwrap_or(""),
        sub
    ));
    let is_bold = BOLD_TOKENS.iter().any(|t| full.contains(t));
    let is_italic = ITALIC_TOKENS.iter().any(|t| full.contains(t));
    if want.wants_bold() != is_bold || want.wants_italic() != is_italic {
        return None;
    }

    let style = squash(sub);
    Some(match want {
        Weight::Regular => {
            if style.is_empty() || ["regular", "roman", "book", "normal"].contains(&style.as_str()) {
                0
            } else {
                1
            }
        }
        Weight::Bold => {
            if style == "bold" {
                0
            } else if style.contains("semibold") || style.contains("demibold") {
                1
            } else if style.contains("heavy") || style.contains("black") {
                2
            } else {
                3
            }
        }
        Weight::Italic => u32::from(!style.contains("italic")),
        Weight::BoldItalic => 1,
    })
}

/// How well a face's family matches one of the names we asked for. `None`
/// means it is a different family altogether; lower is better, and earlier
/// entries in `families` win outright.
fn family_rank(names: &sfnt::Names, families: &[&str]) -> Option<u32> {
    let mut best: Option<u32> = None;
    for (i, want) in families.iter().enumerate() {
        let want = squash(want);
        for candidate in [names.family.as_deref(), names.typographic_family.as_deref()] {
            let Some(candidate) = candidate else { continue };
            let candidate = squash(candidate);
            let extra = if candidate == want {
                0
            } else if candidate.starts_with(&want) {
                // "Microsoft YaHei UI" answers to "Microsoft YaHei".
                1 + (candidate.len() - want.len()) as u32
            } else {
                continue;
            };
            let score = i as u32 * 32 + extra;
            if best.map_or(true, |b| score < b) {
                best = Some(score);
            }
        }
    }
    best
}

fn face_score(names: &sfnt::Names, families: &[&str], want: Option<Weight>) -> Option<u32> {
    let mut score = family_rank(names, families)?;
    if let Some(want) = want {
        score += weight_rank(names, want)?;
    }
    let full = squash(&format!(
        "{} {}",
        names.family.as_deref().unwrap_or(""),
        names.subfamily.as_deref().unwrap_or("")
    ));
    let asked_for_one = families.iter().any(|f| {
        let f = squash(f);
        OFF_WIDTH.iter().any(|t| f.contains(t))
    });
    if !asked_for_one && OFF_WIDTH.iter().any(|t| full.contains(t)) {
        score += 64;
    }
    Some(score)
}

/// Faces of `bytes` in one of `families`, best first, as `(face index, score)`.
/// With `want` set, only faces of that style are returned.
fn rank_faces(bytes: &[u8], families: &[&str], want: Option<Weight>) -> Vec<(u32, u32)> {
    let offsets = sfnt::face_offsets(bytes);
    let offsets = if offsets.is_empty() { vec![0] } else { offsets };
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, base) in offsets.iter().enumerate() {
        let Some(names) = sfnt::names(bytes, *base) else {
            continue;
        };
        if let Some(score) = face_score(&names, families, want) {
            out.push((i as u32, score));
        }
    }
    out.sort_by_key(|(index, score)| (*score, *index));
    out
}

// ===========================================================================
// Where the fonts are, per platform
// ===========================================================================

fn font_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        for dir in [
            "/System/Library/Fonts",
            "/System/Library/Fonts/Supplemental",
            "/Library/Fonts",
        ] {
            v.push(PathBuf::from(dir));
        }
        if let Some(home) = std::env::var_os("HOME") {
            v.push(Path::new(&home).join("Library/Fonts"));
        }
    }

    #[cfg(target_os = "windows")]
    {
        let windows = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        v.push(windows.join("Fonts"));
        // Fonts installed "for me only" live outside the system folder.
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            v.push(
                Path::new(&local)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Fonts"),
            );
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        for dir in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            v.push(PathBuf::from(dir));
        }
        if let Some(home) = std::env::var_os("HOME") {
            let home = Path::new(&home);
            v.push(home.join(".fonts"));
            v.push(home.join(".local/share/fonts"));
        }
    }

    v
}

/// Depth-limited filename search, for fonts that platforms bury in
/// subdirectories.
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

/// Locate a system font file by name.
fn find_font(file: &str) -> Option<PathBuf> {
    let dirs = font_dirs();
    for dir in &dirs {
        let p = dir.join(file);
        if p.is_file() {
            return Some(p);
        }
    }
    // Distributions file their fonts away in per-family subdirectories.
    for dir in &dirs {
        if let Some(p) = find_under(dir, file, 2) {
            return Some(p);
        }
    }
    // macOS keeps some faces in a content-addressed asset store instead.
    #[cfg(target_os = "macos")]
    {
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
    }
    None
}

/// Font bytes, read once and kept for the lifetime of the process — which is
/// how long every face is registered anyway, so an `Arc` dance would only add
/// bookkeeping. It also means a collection used for two weights (regular and
/// bold are the same `PingFang.ttc`, the same `Menlo.ttc`) is read and held
/// once instead of twice.
fn shared_bytes(path: &Path) -> Option<&'static [u8]> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(bytes) = guard.get(path) {
            return Some(bytes);
        }
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 64 {
        return None;
    }
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), leaked);
    }
    Some(leaked)
}

/// Find the face of `candidates` that best matches `want`.
///
/// Two passes: an exact style match anywhere wins, and only if no candidate
/// family has one does the best available face of the right family do. That
/// keeps a machine with, say, only Arial from picking Arial *Regular* while a
/// real bold was sitting in the next file.
fn pick(candidates: &[Family], want: Weight) -> Option<(PathBuf, u32)> {
    let mut loose: Option<(PathBuf, u32, u32)> = None;
    for family in candidates {
        for file in family.files {
            let Some(path) = find_font(file) else { continue };
            let Some(bytes) = shared_bytes(&path) else {
                continue;
            };
            if let Some((index, _)) = rank_faces(bytes, family.names, Some(want)).first() {
                return Some((path, *index));
            }
            for (index, score) in rank_faces(bytes, family.names, None) {
                if loose.as_ref().map_or(true, |(_, _, s)| score < *s) {
                    loose = Some((path.clone(), index, score));
                }
            }
        }
    }
    loose.map(|(path, index, _)| (path, index))
}

/// The four weights of one family.
#[derive(Default)]
struct Resolved {
    regular: Option<(PathBuf, u32)>,
    bold: Option<(PathBuf, u32)>,
    italic: Option<(PathBuf, u32)>,
    bold_italic: Option<(PathBuf, u32)>,
}

fn resolve(candidates: &[Family]) -> Resolved {
    Resolved {
        regular: pick(candidates, Weight::Regular),
        bold: pick(candidates, Weight::Bold),
        italic: pick(candidates, Weight::Italic),
        bold_italic: pick(candidates, Weight::BoldItalic),
    }
}

/// Families the editor only ever asks for upright weights of: CJK has no
/// italic, and the code blocks have no bold-italic family.
fn resolve_upright(candidates: &[Family]) -> Resolved {
    Resolved {
        regular: pick(candidates, Weight::Regular),
        bold: pick(candidates, Weight::Bold),
        ..Default::default()
    }
}

// ===========================================================================
// What each platform actually ships
// ===========================================================================

mod table {
    use super::Family;

    // ---- macOS -------------------------------------------------------------

    #[cfg(target_os = "macos")]
    pub const SANS: &[Family] = &[
        Family::new(&["HelveticaNeue.ttc"], &["Helvetica Neue"]),
        Family::new(&["Helvetica.ttc"], &["Helvetica"]),
        Family::new(
            &[
                "Arial.ttf",
                "Arial Bold.ttf",
                "Arial Italic.ttf",
                "Arial Bold Italic.ttf",
            ],
            &["Arial"],
        ),
        Family::new(&["Geneva.ttf"], &["Geneva"]),
        Family::new(&["Arial Unicode.ttf"], &["Arial Unicode MS"]),
    ];

    #[cfg(target_os = "macos")]
    pub const MONO: &[Family] = &[
        Family::new(&["Menlo.ttc"], &["Menlo"]),
        Family::new(&["Monaco.ttf"], &["Monaco"]),
        Family::new(&["SFNSMono.ttf"], &["SF Mono", ".SF NS Mono"]),
        Family::new(&["Courier.ttc"], &["Courier"]),
    ];

    #[cfg(target_os = "macos")]
    pub const CJK: &[Family] = &[
        Family::new(&["PingFang.ttc"], &["PingFang SC", "PingFang HK", "PingFang TC"]),
        Family::new(&["Hiragino Sans GB.ttc"], &["Hiragino Sans GB"]),
        Family::new(&["STHeiti Medium.ttc"], &["Heiti SC", "STHeiti"]),
        Family::new(&["Songti.ttc"], &["Songti SC", "STSong"]),
        Family::new(&["Arial Unicode.ttf"], &["Arial Unicode MS"]),
    ];

    #[cfg(target_os = "macos")]
    pub const SERIF: &[Family] = &[
        Family::new(&["STIXTwoText.ttf"], &["STIX Two Text"]),
        Family::new(&["Georgia.ttf"], &["Georgia"]),
        Family::new(&["Times New Roman.ttf"], &["Times New Roman"]),
        Family::new(&["Times.ttc"], &["Times"]),
    ];

    #[cfg(target_os = "macos")]
    pub const MATH: &[Family] = &[
        Family::new(&["STIXTwoMath.otf"], &["STIX Two Math"]),
        Family::new(&["STIXGeneral.otf"], &["STIXGeneral"]),
        Family::new(&["Apple Symbols.ttf"], &["Apple Symbols"]),
    ];

    #[cfg(target_os = "macos")]
    pub const MATH_ITALIC: &[Family] = &[
        Family::new(&["STIXTwoText-Italic.ttf"], &["STIX Two Text"]),
        Family::new(&["STIXGeneralItalic.otf"], &["STIXGeneral"]),
    ];

    // ---- Windows -----------------------------------------------------------
    //
    // One file per weight. Segoe UI and Consolas ship with every Windows since
    // 7; Arial and Courier New are the fallbacks that have been there longer.
    // The Chinese entries are what matters: `msyh.ttc` holds two faces and is
    // resolved by name, so the ordering inside it cannot bite.

    #[cfg(target_os = "windows")]
    pub const SANS: &[Family] = &[
        Family::new(
            &["segoeui.ttf", "segoeuib.ttf", "segoeuii.ttf", "segoeuiz.ttf"],
            &["Segoe UI"],
        ),
        Family::new(
            &["arial.ttf", "arialbd.ttf", "ariali.ttf", "arialbi.ttf"],
            &["Arial"],
        ),
        Family::new(&["tahoma.ttf", "tahomabd.ttf"], &["Tahoma"]),
        Family::new(
            &["verdana.ttf", "verdanab.ttf", "verdanai.ttf", "verdanaz.ttf"],
            &["Verdana"],
        ),
        Family::new(
            &["calibri.ttf", "calibrib.ttf", "calibrii.ttf", "calibriz.ttf"],
            &["Calibri"],
        ),
    ];

    #[cfg(target_os = "windows")]
    pub const MONO: &[Family] = &[
        Family::new(
            &["consola.ttf", "consolab.ttf", "consolai.ttf", "consolaz.ttf"],
            &["Consolas"],
        ),
        Family::new(
            &["cour.ttf", "courbd.ttf", "couri.ttf", "courbi.ttf"],
            &["Courier New"],
        ),
        Family::new(&["lucon.ttf"], &["Lucida Console"]),
    ];

    #[cfg(target_os = "windows")]
    pub const CJK: &[Family] = &[
        Family::new(
            &["msyh.ttc", "msyhbd.ttc", "msyhl.ttc"],
            &["Microsoft YaHei", "Microsoft YaHei UI"],
        ),
        Family::new(&["msyh.ttf", "msyhbd.ttf"], &["Microsoft YaHei"]),
        Family::new(&["Deng.ttf", "dengb.ttf", "dengl.ttf"], &["DengXian"]),
        Family::new(&["simhei.ttf"], &["SimHei"]),
        Family::new(&["simsun.ttc"], &["SimSun", "NSimSun"]),
        Family::new(&["simkai.ttf"], &["KaiTi", "SimKai"]),
        Family::new(&["msjh.ttc", "msjhbd.ttc"], &["Microsoft JhengHei", "Microsoft JhengHei UI"]),
        // Reached only when no Chinese face is installed at all — a Japanese or
        // Korean Windows without the Chinese language pack. Kana and hangul are
        // then better served by these than by tofu.
        Family::new(&["malgun.ttf", "malgunbd.ttf"], &["Malgun Gothic"]),
        Family::new(&["meiryo.ttc", "meiryob.ttc"], &["Meiryo", "Meiryo UI"]),
        Family::new(&["msgothic.ttc"], &["MS Gothic", "MS PGothic", "MS UI Gothic"]),
    ];

    #[cfg(target_os = "windows")]
    pub const SERIF: &[Family] = &[
        Family::new(
            &["times.ttf", "timesbd.ttf", "timesi.ttf", "timesbi.ttf"],
            &["Times New Roman"],
        ),
        Family::new(
            &["georgia.ttf", "georgiab.ttf", "georgiai.ttf", "georgiaz.ttf"],
            &["Georgia"],
        ),
        Family::new(
            &["constan.ttf", "constanb.ttf", "constani.ttf", "constanz.ttf"],
            &["Constantia"],
        ),
        Family::new(&["cambria.ttc", "cambriab.ttf"], &["Cambria"]),
    ];

    #[cfg(target_os = "windows")]
    pub const MATH: &[Family] = &[
        Family::new(&["cambria.ttc"], &["Cambria Math"]),
        Family::new(&["STIXTwoMath.otf"], &["STIX Two Math"]),
        Family::new(&["seguisym.ttf"], &["Segoe UI Symbol"]),
    ];

    #[cfg(target_os = "windows")]
    pub const MATH_ITALIC: &[Family] = &[
        Family::new(&["STIXTwoText-Italic.ttf"], &["STIX Two Text"]),
        Family::new(&["cambria.ttc"], &["Cambria Math"]),
    ];

    // ---- everything else ---------------------------------------------------

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const SANS: &[Family] = &[
        Family::new(
            &[
                "DejaVuSans.ttf",
                "DejaVuSans-Bold.ttf",
                "DejaVuSans-Oblique.ttf",
                "DejaVuSans-BoldOblique.ttf",
            ],
            &["DejaVu Sans"],
        ),
        Family::new(
            &[
                "LiberationSans-Regular.ttf",
                "LiberationSans-Bold.ttf",
                "LiberationSans-Italic.ttf",
                "LiberationSans-BoldItalic.ttf",
            ],
            &["Liberation Sans"],
        ),
        Family::new(
            &[
                "NotoSans-Regular.ttf",
                "NotoSans-Bold.ttf",
                "NotoSans-Italic.ttf",
                "NotoSans-BoldItalic.ttf",
            ],
            &["Noto Sans"],
        ),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const MONO: &[Family] = &[
        Family::new(
            &[
                "DejaVuSansMono.ttf",
                "DejaVuSansMono-Bold.ttf",
                "DejaVuSansMono-Oblique.ttf",
            ],
            &["DejaVu Sans Mono"],
        ),
        Family::new(
            &[
                "LiberationMono-Regular.ttf",
                "LiberationMono-Bold.ttf",
                "LiberationMono-Italic.ttf",
            ],
            &["Liberation Mono"],
        ),
        Family::new(
            &["NotoSansMono-Regular.ttf", "NotoSansMono-Bold.ttf"],
            &["Noto Sans Mono"],
        ),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const CJK: &[Family] = &[
        Family::new(
            &[
                "NotoSansCJK-Regular.ttc",
                "NotoSansCJK-Bold.ttc",
                "NotoSansCJKsc-Regular.otf",
                "NotoSansCJKsc-Bold.otf",
            ],
            &["Noto Sans CJK SC", "Noto Sans CJK JP", "Source Han Sans SC"],
        ),
        Family::new(
            &["wqy-microhei.ttc", "wqy-zenhei.ttc"],
            &["WenQuanYi Micro Hei", "WenQuanYi Zen Hei"],
        ),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const SERIF: &[Family] = &[
        Family::new(
            &[
                "DejaVuSerif.ttf",
                "DejaVuSerif-Bold.ttf",
                "DejaVuSerif-Italic.ttf",
            ],
            &["DejaVu Serif"],
        ),
        Family::new(
            &["LiberationSerif-Regular.ttf", "LiberationSerif-Bold.ttf"],
            &["Liberation Serif"],
        ),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const MATH: &[Family] = &[
        Family::new(&["STIXTwoMath-Regular.otf", "STIXTwoMath.otf"], &["STIX Two Math"]),
        Family::new(&["DejaVuSans.ttf"], &["DejaVu Sans"]),
    ];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    pub const MATH_ITALIC: &[Family] = &[
        Family::new(&["STIXTwoText-Italic.otf"], &["STIX Two Text"]),
        Family::new(&["DejaVuSerif-Italic.ttf"], &["DejaVu Serif"]),
    ];
}

// ===========================================================================
// Installing the stack
// ===========================================================================

/// Which system fonts were found, so the About panel can say so.
#[derive(Debug, Clone, Default)]
pub struct FontReport {
    pub resolved: Vec<(&'static str, String)>,
    pub missing: Vec<&'static str>,
}

impl FontReport {
    #[allow(dead_code)]
    pub fn summary(&self) -> String {
        let mut s = String::new();
        for (key, path) in &self.resolved {
            s.push_str(&format!("  {key:<16} {path}\n"));
        }
        for key in &self.missing {
            s.push_str(&format!("  {key:<16} (missing, using built-in)\n"));
        }
        s
    }
}

#[derive(Default)]
struct Builder {
    defs: FontDefinitions,
    report: FontReport,
}

impl Builder {
    fn add(&mut self, key: &'static str, path: &Path, index: u32) -> bool {
        let Some(bytes) = shared_bytes(path) else {
            return false;
        };
        let mut data = FontData::from_static(bytes);
        data.index = index;
        self.defs.font_data.insert(key.to_owned(), Arc::new(data));
        self.report
            .resolved
            .push((key, format!("{}#{}", path.display(), index)));
        true
    }

    /// Register one slot and report back the keys it contributed.
    fn slot(&mut self, key: &'static str, picked: Option<&(PathBuf, u32)>) -> Vec<&'static str> {
        match picked {
            Some((path, index)) if self.add(key, path, *index) => vec![key],
            _ => Vec::new(),
        }
    }
}

/// Install the full font stack into the egui context.
pub fn install(ctx: &egui::Context) -> FontReport {
    let mut b = Builder::default();

    let sans = resolve(table::SANS);
    let mono = resolve(table::MONO);
    let cjk = resolve_upright(table::CJK);
    let serif = resolve(table::SERIF);
    let math = resolve(table::MATH);
    let math_italic = resolve(table::MATH_ITALIC);

    let sans_keys = b.slot("sans", sans.regular.as_ref());
    let bold_keys = b.slot("sans-bold", sans.bold.as_ref());
    let italic_keys = b.slot("sans-italic", sans.italic.as_ref());
    let bold_italic_keys = b.slot("sans-bold-italic", sans.bold_italic.as_ref());
    let mono_keys = b.slot("mono", mono.regular.as_ref());
    let mono_bold_keys = b.slot("mono-bold", mono.bold.as_ref());
    let cjk_keys = b.slot("cjk", cjk.regular.as_ref());
    let cjk_bold_keys = b.slot("cjk-bold", cjk.bold.as_ref());
    let serif_keys = b.slot("serif", serif.regular.as_ref());
    let math_keys = b.slot("math", math.regular.as_ref());
    let math_italic_keys = b.slot("math-italic", math_italic.italic.as_ref());

    for (name, found) in [
        ("sans", !sans_keys.is_empty()),
        ("sans-italic", !italic_keys.is_empty()),
        ("mono", !mono_keys.is_empty()),
        // The one that matters: without it every label in the interface is tofu.
        ("cjk", !cjk_keys.is_empty()),
        ("serif", !serif_keys.is_empty()),
        ("math", !math_keys.is_empty()),
    ] {
        if !found {
            b.report.missing.push(name);
        }
    }

    // ---- build the fallback chains -----------------------------------------
    //
    // egui's own faces are the last resort for anything still missing. "Hack"
    // is egui's built-in monospace: without it a missing system mono would fall
    // back to a proportional face and code blocks would stop lining up.
    const UI_TAIL: &[&str] = &["NotoEmoji-Regular", "emoji-icon-font", "Ubuntu-Light"];
    const MONO_TAIL: &[&str] = &["Hack", "NotoEmoji-Regular", "emoji-icon-font"];

    let chain = |parts: Vec<Vec<&'static str>>, tail: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for seg in parts {
            for k in seg {
                if !v.iter().any(|e| e == k) {
                    v.push(k.to_string());
                }
            }
        }
        for k in tail {
            if !v.iter().any(|e| e == k) {
                v.push((*k).to_string());
            }
        }
        v
    };

    let proportional = chain(
        vec![sans_keys.clone(), cjk_keys.clone(), mono_keys.clone()],
        UI_TAIL,
    );
    let monospace = chain(
        vec![mono_keys.clone(), cjk_keys.clone(), sans_keys.clone()],
        MONO_TAIL,
    );
    let bold = chain(
        vec![
            bold_keys.clone(),
            cjk_bold_keys.clone(),
            cjk_keys.clone(),
            sans_keys.clone(),
        ],
        UI_TAIL,
    );
    let italic = chain(
        vec![italic_keys.clone(), sans_keys.clone(), cjk_keys.clone()],
        UI_TAIL,
    );
    let bold_italic = chain(
        vec![
            bold_italic_keys.clone(),
            bold_keys.clone(),
            italic_keys.clone(),
            sans_keys.clone(),
            cjk_bold_keys.clone(),
        ],
        UI_TAIL,
    );
    let mono_bold = chain(
        vec![mono_bold_keys.clone(), mono_keys.clone(), cjk_bold_keys.clone()],
        MONO_TAIL,
    );
    let serif = chain(
        vec![serif_keys.clone(), cjk_keys.clone(), sans_keys.clone()],
        UI_TAIL,
    );
    let math = chain(
        vec![
            math_keys.clone(),
            serif_keys.clone(),
            sans_keys.clone(),
            cjk_keys.clone(),
        ],
        UI_TAIL,
    );
    let math_italic = chain(
        vec![
            math_italic_keys.clone(),
            math_keys.clone(),
            serif_keys.clone(),
            sans_keys.clone(),
            // A formula is allowed to contain Chinese; without this every
            // italicised run of it would be tofu.
            cjk_keys.clone(),
        ],
        UI_TAIL,
    );

    let named: Vec<(&str, Vec<String>)> = vec![
        ("bold", bold),
        ("italic", italic),
        ("bold-italic", bold_italic),
        ("mono-bold", mono_bold),
        ("serif", serif),
        ("math", math),
        ("mathit", math_italic),
    ];
    b.defs
        .families
        .insert(FontFamily::Proportional, proportional);
    b.defs.families.insert(FontFamily::Monospace, monospace);
    for (name, list) in named {
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
    format!(
        "正文 {} · 中文 {} · 等宽 {}",
        pick("sans"),
        pick("cjk"),
        pick("mono")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interface is Chinese, and a picker drawn with a font that has no CJK
    /// glyphs is a wall of tofu boxes that looks like a broken app. This is the
    /// assertion that catches it, on whichever platform the tests run — the
    /// Windows runner in CI included.
    #[test]
    fn every_family_can_draw_the_interface_language() {
        let ctx = egui::Context::default();
        let report = install(&ctx);
        // egui only builds its glyph atlases inside a pass, so the question
        // "does this family have the glyph?" cannot be asked of a context that
        // has never run.
        let _ = ctx.run(Default::default(), |_| {});
        let families = [
            ("proportional", FontFamily::Proportional),
            ("monospace", FontFamily::Monospace),
            ("bold", family_for(true, false)),
            ("italic", family_for(false, true)),
            ("bold-italic", family_for(true, true)),
            ("mono-bold", mono_family_for(true)),
            ("serif", serif_family()),
            ("math", math_family()),
            ("mathit", math_italic_family()),
        ];
        let samples = [
            ("Chinese", "中文测试，标点符号。《引用》"),
            ("Latin", "The quick brown fox"),
        ];
        let mut broken = Vec::new();
        for (name, family) in families {
            for (script, sample) in samples {
                let id = egui::FontId::new(14.0, family.clone());
                if !ctx.fonts(|f| f.has_glyphs(&id, sample)) {
                    broken.push(format!("{name} cannot draw {script}"));
                }
            }
        }
        assert!(
            broken.is_empty(),
            "{}\nresolved fonts:\n{}",
            broken.join("; "),
            report.summary()
        );
    }

    /// A `.ttc` face index is a lookup, not a guess: on Windows the Chinese
    /// families only exist as collections.
    #[test]
    fn a_face_is_found_by_name_not_by_position() {
        let bytes = std::fs::read(fixture()).expect("no font to read");
        let offsets = sfnt::face_offsets(&bytes);
        assert!(offsets.len() > 1, "expected a collection");
        // Every face in a collection can be described, and the description is
        // what the match is made on.
        let mut families = Vec::new();
        for base in &offsets {
            let names = sfnt::names(&bytes, *base).expect("a face without a name table");
            families.push(names.family.unwrap_or_default());
        }
        assert!(families.iter().all(|f| !f.is_empty()), "{families:?}");
    }

    /// A font file that is not a font must not be mistaken for one.
    #[test]
    fn junk_is_not_a_font() {
        assert!(sfnt::face_offsets(b"not a font at all, honestly").is_empty());
        assert!(sfnt::face_offsets(&[]).is_empty());
        assert!(sfnt::names(b"\x00\x01\x00\x00junkjunkjunk", 0).is_none());
    }

    /// macOS behaviour is pinned: these are the faces the editor has always
    /// used, and the name lookups above exist to reproduce them, not to change
    /// them.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_mac_faces_are_the_ones_that_were_hard_coded() {
        let stem = |p: &(PathBuf, u32)| {
            (
                p.0.file_name().unwrap().to_string_lossy().into_owned(),
                p.1,
            )
        };
        assert_eq!(
            pick(table::SANS, Weight::Regular).as_ref().map(stem),
            Some(("HelveticaNeue.ttc".into(), 0))
        );
        assert_eq!(
            pick(table::SANS, Weight::Bold).as_ref().map(stem),
            Some(("HelveticaNeue.ttc".into(), 1))
        );
        assert_eq!(
            pick(table::SANS, Weight::Italic).as_ref().map(stem),
            Some(("HelveticaNeue.ttc".into(), 2))
        );
        assert_eq!(
            pick(table::SANS, Weight::BoldItalic).as_ref().map(stem),
            Some(("HelveticaNeue.ttc".into(), 3))
        );
        assert_eq!(
            pick(table::MONO, Weight::Regular).as_ref().map(stem),
            Some(("Menlo.ttc".into(), 0))
        );
        assert_eq!(
            pick(table::MONO, Weight::Bold).as_ref().map(stem),
            Some(("Menlo.ttc".into(), 1))
        );
        let (path, _) = pick(table::CJK, Weight::Regular).expect("no Chinese font");
        assert_eq!(path.file_name().unwrap(), "PingFang.ttc");
    }

    /// On Windows the index inside `msyh.ttc` has to come out of the name
    /// table. Runs on the CI runner, where the answer is known to be a real
    /// Chinese face rather than whichever one happened to be first.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_finds_a_chinese_face_by_name() {
        let (path, index) = pick(table::CJK, Weight::Regular).expect("no Chinese font installed");
        let bytes = shared_bytes(&path).unwrap();
        let names = sfnt::names(bytes, sfnt::face_offsets(bytes)[index as usize]).unwrap();
        println!("cjk regular: {}#{} = {names:?}", path.display(), index);
        assert!(
            family_rank(&names, table::CJK[0].names).is_some(),
            "{names:?}"
        );
        let (bold_path, bold_index) = pick(table::CJK, Weight::Bold).expect("no bold Chinese font");
        let bold_bytes = shared_bytes(&bold_path).unwrap();
        let bold_names =
            sfnt::names(bold_bytes, sfnt::face_offsets(bold_bytes)[bold_index as usize]).unwrap();
        println!(
            "cjk bold:    {}#{} = {bold_names:?}",
            bold_path.display(),
            bold_index
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_finds_its_system_ui_face() {
        let (path, _) = pick(table::SANS, Weight::Regular).expect("no UI font");
        println!("sans: {}", path.display());
        assert!(path.exists());
    }

    fn fixture() -> PathBuf {
        #[cfg(target_os = "macos")]
        {
            PathBuf::from("/System/Library/Fonts/Menlo.ttc")
        }
        #[cfg(target_os = "windows")]
        {
            let windows = std::env::var_os("WINDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            windows.join("Fonts").join("msyh.ttc")
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            PathBuf::from("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
        }
    }
}
