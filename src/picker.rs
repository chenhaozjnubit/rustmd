//! A built-in file browser.
//!
//! Relying on the OS file panel alone turned out to be fragile: `rfd`'s macOS
//! backend still drives `NSSavePanel.setAllowedFileTypes`, an API Apple
//! deprecated in macOS 12, and when the panel fails to resolve those
//! extensions it greys out *every* file — including the `.md` files this
//! program exists to open. So opening a document must not depend on it.
//!
//! This module is a self-contained picker: it reads directories itself, filters
//! to Markdown by default, and never blocks. The listing is cached and only
//! re-read when the directory actually changes, so it costs nothing per frame.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Extensions treated as Markdown when filtering.
pub const MARKDOWN_EXTS: &[&str] = &["md", "markdown", "mdown", "mkd", "mdwn", "mdx"];

/// Extensions we happily open, but do not advertise as Markdown.
pub const TEXT_EXTS: &[&str] = &["txt", "text", "rst", "org"];

/// True when the path looks like a Markdown document.
pub fn is_markdown(path: &Path) -> bool {
    ext_of(path).map_or(false, |e| MARKDOWN_EXTS.contains(&e.as_str()))
}

/// True when the path is something a text editor should open at all.
pub fn is_textish(path: &Path) -> bool {
    ext_of(path)
        .map(|e| MARKDOWN_EXTS.contains(&e.as_str()) || TEXT_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
}

/// One row of the listing.
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    /// Seconds since the epoch, for the "modified" column.
    pub modified: Option<u64>,
}

/// A directory listing, cached until something actually changes.
#[derive(Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    /// Set when the directory held more entries than we were willing to list.
    pub truncated: bool,
    pub error: Option<String>,
}

/// How many rows we are willing to hold for one directory. A folder with more
/// entries than this is a filesystem root, not a document collection.
const MAX_ENTRIES: usize = 4000;

/// Built-in file browser state.
#[derive(Debug)]
pub struct FilePicker {
    pub open: bool,
    /// True when the picker was opened to choose a folder instead of a file.
    pub picking_folder: bool,
    pub dir: PathBuf,
    pub listing: Listing,
    pub show_all: bool,
    pub filter: String,
    /// Index into `visible()` of the highlighted row.
    pub cursor: usize,
    /// The editable path bar; kept in sync with `dir` unless being typed into.
    pub path_buf: String,
    /// Set by the user; causes the listing to be re-read on the next frame.
    need_refresh: bool,
}

impl Default for FilePicker {
    fn default() -> Self {
        Self {
            open: false,
            picking_folder: false,
            dir: default_dir(),
            listing: Listing::default(),
            show_all: false,
            filter: String::new(),
            cursor: 0,
            path_buf: String::new(),
            need_refresh: true,
        }
    }
}

impl FilePicker {
    /// Open on a file, starting in `dir` when it exists.
    pub fn open_at(&mut self, dir: Option<PathBuf>) {
        self.open = true;
        self.picking_folder = false;
        if let Some(d) = dir.filter(|d| d.is_dir()) {
            self.set_dir(d);
        }
        self.refresh();
        self.cursor = 0;
    }

    /// Open as a folder chooser, for "打开文件夹…".
    pub fn open_folder_at(&mut self, dir: Option<PathBuf>) {
        self.open_at(dir);
        self.picking_folder = true;
        self.show_all = true;
        self.refresh();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.picking_folder = false;
        self.filter.clear();
    }

    fn set_dir(&mut self, dir: PathBuf) {
        if dir != self.dir {
            self.dir = dir;
            self.cursor = 0;
        }
        self.refresh();
    }

    /// Ask for the listing to be re-read on the next frame.
    pub fn refresh(&mut self) {
        self.need_refresh = true;
        self.path_buf = self.dir.display().to_string();
    }

    /// Does this entry survive the filter text?
    pub fn matches(&self, e: &Entry) -> bool {
        let f = self.filter.trim();
        f.is_empty() || e.name.to_ascii_lowercase().contains(&f.to_ascii_lowercase())
    }

    /// Indices into `listing.entries` of the rows to show, in display order.
    ///
    /// Indices rather than clones: the listing can hold thousands of entries
    /// and this is recomputed every frame.
    pub fn visible(&self) -> Vec<usize> {
        self.listing
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| self.matches(e))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn go(&mut self, dir: PathBuf) {
        if !dir.is_dir() {
            return;
        }
        self.set_dir(dir);
        self.cursor = 0;
    }

    pub fn go_up(&mut self) {
        if let Some(p) = self.dir.parent() {
            let p = p.to_path_buf();
            self.go(p);
        }
    }

    pub fn go_home(&mut self) {
        self.go(default_dir());
    }

    pub fn selected(&self) -> Option<Entry> {
        let vis = self.visible();
        vis.get(self.cursor)
            .and_then(|i| self.listing.entries.get(*i))
            .cloned()
    }

    /// Re-read the directory if anything invalidated the cache.
    ///
    /// The highlight is clamped on every call, not just when the listing is
    /// re-read: typing in the filter box can shrink `visible()` without
    /// touching the directory at all.
    pub fn tick(&mut self) {
        if self.need_refresh {
            self.need_refresh = false;
            self.listing = read_listing(&self.dir, self.show_all);
        }
        let n = self.visible().len();
        if self.cursor >= n {
            self.cursor = n.saturating_sub(1);
        }
    }

    /// Toggle the "show everything" switch and re-read.
    pub fn set_show_all(&mut self, on: bool) {
        if self.show_all != on {
            self.show_all = on;
            self.refresh();
        }
    }
}

/// Read one directory into a sorted listing.
pub fn read_listing(dir: &Path, show_all: bool) -> Listing {
    let mut out = Listing::default();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            out.error = Some(describe_io(&e));
            return out;
        }
    };
    let mut dirs: Vec<Entry> = Vec::new();
    let mut files: Vec<Entry> = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && !show_all {
            continue;
        }
        let Ok(ft) = ent.file_type() else { continue };
        let path = ent.path();
        let meta = ent.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(to_unix);
        if ft.is_dir() {
            dirs.push(Entry {
                name,
                path,
                is_dir: true,
                size: 0,
                modified,
            });
        } else if ft.is_file() {
            if !show_all && !is_textish(&path) {
                continue;
            }
            files.push(Entry {
                name,
                path,
                is_dir: false,
                size,
                modified,
            });
        }
        if dirs.len() + files.len() >= MAX_ENTRIES {
            out.truncated = true;
            break;
        }
    }
    let key = |a: &Entry, b: &Entry| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    dirs.sort_by(key);
    files.sort_by(key);
    dirs.extend(files);
    out.entries = dirs;
    out
}

fn to_unix(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// `$HOME`, falling back to the current directory.
pub fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn describe_io(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => "没有访问权限".into(),
        std::io::ErrorKind::NotFound => "目录不存在".into(),
        _ => e.to_string(),
    }
}

/// `1.2 MB`, `48 KB`, `312 B` — the compact form used in the listing.
pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.2} GB", b / (KB * KB * KB))
    }
}

/// `2026-09-19 20:31` in local time, computed without pulling in a date crate.
///
/// The offset comes from the C library, which is the only place the local
/// timezone is authoritatively known.
pub fn human_time(unix: u64) -> String {
    if unix == 0 {
        return String::new();
    }
    let secs = unix as i64 + local_offset_secs(unix as i64);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// Seconds to add to UTC to get local time, via `localtime_r`.
#[cfg(unix)]
fn local_offset_secs(unix: i64) -> i64 {
    // `libc` is pulled in by other dependencies; declare just the one call we
    // need so there is no extra dependency of our own.
    #[repr(C)]
    struct Tm {
        tm_sec: i32,
        tm_min: i32,
        tm_hour: i32,
        tm_mday: i32,
        tm_mon: i32,
        tm_year: i32,
        tm_wday: i32,
        tm_yday: i32,
        tm_isdst: i32,
        tm_gmtoff: i64,
        tm_zone: *const i8,
    }
    unsafe extern "C" {
        fn localtime_r(timep: *const i64, result: *mut Tm) -> *mut Tm;
        fn tzset();
    }
    unsafe {
        tzset();
        let mut tm: Tm = std::mem::zeroed();
        if localtime_r(&unix, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff
        }
    }
}

#[cfg(not(unix))]
fn local_offset_secs(_unix: i64) -> i64 {
    0
}

/// Days since the epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_markdown() {
        assert!(is_markdown(Path::new("/tmp/a.md")));
        assert!(is_markdown(Path::new("/tmp/a.MARKDOWN")));
        assert!(is_textish(Path::new("/tmp/a.txt")));
        assert!(!is_markdown(Path::new("/tmp/a.rs")));
        assert!(!is_textish(Path::new("/tmp/a.png")));
        assert!(!is_markdown(Path::new("/tmp/a")));
    }

    /// A scratch directory holding a known mix of files.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustmd-picker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).expect("scratch dir");
        for (name, body) in [
            ("a.md", "# a"),
            ("b.MARKDOWN", "# b"),
            ("c.txt", "c"),
            ("d.png", "not really a png"),
            (".hidden.md", "# hidden"),
            ("sub/e.md", "# e"),
        ] {
            std::fs::write(dir.join(name), body).expect("write fixture");
        }
        dir
    }

    #[test]
    fn the_default_listing_offers_exactly_the_openable_files() {
        let dir = scratch("filter");
        let md = read_listing(&dir, false);
        let names: Vec<&str> = md.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.md"));
        assert!(
            names.contains(&"b.MARKDOWN"),
            "extension matching must ignore case"
        );
        assert!(names.contains(&"c.txt"), "plain text is still openable");
        assert!(names.contains(&"sub"), "directories are always listed");
        assert!(!names.contains(&"d.png"), "a png is not a document");
        assert!(
            !names.contains(&".hidden.md"),
            "dotfiles are hidden by default"
        );

        let all = read_listing(&dir, true);
        assert!(all.entries.iter().any(|e| e.name == "d.png"));
        assert!(all.entries.iter().any(|e| e.name == ".hidden.md"));

        // Directories come before files.
        let first_file = all.entries.iter().position(|e| !e.is_dir).unwrap();
        let last_dir = all.entries.iter().rposition(|e| e.is_dir).unwrap();
        assert!(last_dir < first_file, "all directories should come first");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_real_source_directory_still_lists_without_dotfiles() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let all = read_listing(&dir, true);
        assert!(all.error.is_none(), "unexpected error: {:?}", all.error);
        assert!(all.entries.iter().any(|e| e.name == "main.rs"));
        assert!(all.entries.iter().all(|e| !e.name.starts_with('.')));
    }

    #[test]
    fn sizes_read_well() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn civil_dates_are_right() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2026-09-19 is 20715 days after the epoch.
        assert_eq!(civil_from_days(20715), (2026, 9, 19));
        // A leap day.
        assert_eq!(civil_from_days(19782), (2024, 2, 29));
        // And the day before the epoch, which exercises the negative branch.
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        // A century that is not a leap year.
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
    }

    #[test]
    fn the_filter_narrows_the_listing() {
        let dir = scratch("narrow");
        let mut p = FilePicker::default();
        p.go(dir.clone());
        p.tick();
        let all = p.visible().len();
        assert!(all >= 4, "expected the fixtures plus sub/, got {all}");

        p.filter = "a".into();
        let hits = p.visible();
        assert!(!hits.is_empty());
        assert!(hits.len() < all);
        assert!(hits.iter().all(|i| {
            p.listing.entries[*i]
                .name
                .to_ascii_lowercase()
                .contains('a')
        }));

        p.filter = "zzz".into();
        assert!(p.visible().is_empty());

        p.filter.clear();
        assert_eq!(p.visible().len(), all);

        // Turning the filter off brings in the non-text files.
        let before = p.listing.entries.len();
        p.set_show_all(true);
        p.tick();
        assert!(p.listing.entries.len() > before);
        assert!(p.listing.entries.iter().any(|e| e.name == "d.png"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn walking_in_and_out_of_directories() {
        let dir = scratch("walk");
        let mut p = FilePicker::default();
        p.open_at(Some(dir.clone()));
        p.tick();
        assert_eq!(p.dir, dir);
        assert!(!p.listing.entries.is_empty());
        assert!(p.selected().is_some(), "the first row must be selectable");

        // Deselecting down to the last row and back must not overflow.
        p.cursor = p.visible().len() - 1;
        p.filter = "zzz".into();
        p.tick();
        assert_eq!(p.cursor, 0, "an empty listing clamps the cursor to zero");

        p.filter.clear();
        p.tick();
        p.go(dir.join("sub"));
        p.tick();
        assert_eq!(p.dir, dir.join("sub"));
        assert_eq!(p.cursor, 0, "entering a directory re-highlights the first row");

        p.go_up();
        p.tick();
        assert_eq!(p.dir, dir);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn going_up_from_the_root_is_a_no_op() {
        let mut p = FilePicker::default();
        p.go(PathBuf::from("/"));
        p.tick();
        p.go_up();
        assert_eq!(p.dir, PathBuf::from("/"));
    }

    #[test]
    fn a_bad_directory_reports_an_error_instead_of_panicking() {
        let l = read_listing(Path::new("/definitely/not/here"), true);
        assert!(l.entries.is_empty());
        assert!(l.error.is_some());
    }
}
