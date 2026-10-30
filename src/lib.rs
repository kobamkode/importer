//! Import core: copies media files from a Source into a date-organized Destination.
//!
//! No UI, no gpui — the shell wraps this. Domain terms: see `GLOSSARY.md`.
//! Two test seams live here: [`import_files`] (the core entry point) and
//! [`taken_date`] (the Taken Date parser).

use std::path::{Path, PathBuf};

use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use nom_exif::{ExifTag, Metadata, TrackInfoTag, read_metadata};

/// User-configured preferences that drive where and how files are written.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Root of the Destination; date folders are created under it as needed.
    pub dest_root: PathBuf,
    /// Token format (`YYYYMMDD`, `YYYY-MM-DD`, `YYYYMMDD_HHMMSS`) driving
    /// both the date folder name and the file-name prefix.
    pub date_format: String,
}

impl Default for Settings {
    fn default() -> Self {
        let dest_root = std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Pictures").join("Imports"))
            .unwrap_or_else(|| PathBuf::from("Imports"));
        Self {
            dest_root,
            date_format: "YYYYMMDD".to_string(),
        }
    }
}

/// What one run of the entry point produced.
#[derive(Debug, Default)]
pub struct Report {
    /// `(source, destination)` for every file that was copied.
    pub mappings: Vec<(PathBuf, PathBuf)>,
    pub imported: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Where settings persist: `$XDG_CONFIG_HOME/importer/settings.conf`, falling
/// back to `~/.config/importer/settings.conf`.
///
/// shortcut: flat `key=value` lines — enough while settings are two strings;
/// switch to a serialized format (toml + serde) when a setting arrives that
/// isn't a flat string.
pub fn settings_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default();
    base.join("importer").join("settings.conf")
}

/// Read settings, layering values over [`Settings::default`]. A missing file
/// or a missing key falls back per-key; there is no env-var override.
pub fn load_settings() -> Settings {
    load_settings_from(&settings_path())
}

/// Load from an explicit path (test seam; `load_settings` uses [`settings_path`]).
pub fn load_settings_from(path: &Path) -> Settings {
    let mut settings = Settings::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return settings;
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue; // unparseable line: leave that key at its default
        };
        let (key, value) = (key.trim(), value.trim());
        // Empty values are rejected at this boundary rather than producing a
        // date folder called "" later.
        match key {
            "dest_root" if !value.is_empty() => settings.dest_root = PathBuf::from(value),
            "date_format" if !value.is_empty() => settings.date_format = value.to_string(),
            _ => {} // unknown keys: forward-compatible, ignored
        }
    }
    settings
}

pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    save_settings_to(&settings_path(), settings)
}

/// Save to an explicit path (test seam; `save_settings` uses [`settings_path`]).
pub fn save_settings_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        format!(
            "dest_root={}\ndate_format={}\n",
            settings.dest_root.display(),
            settings.date_format
        ),
    )
}

/// The moment a photo or video was captured: EXIF `DateTimeOriginal` (images)
/// or `CreateDate` (video), falling back to the file's modified-time when the
/// file carries neither. Returns `None` only when the file can't be read at all.
pub fn taken_date(path: &Path) -> Option<NaiveDateTime> {
    metadata_date(path).or_else(|| mtime_date(path))
}

fn metadata_date(path: &Path) -> Option<NaiveDateTime> {
    // Camera clocks carry no timezone for EXIF and a fixed one for video;
    // timezone-perfect grouping is explicitly out of scope (spec).
    match read_metadata(path).ok()? {
        Metadata::Exif(exif) => exif.get(ExifTag::DateTimeOriginal)?.as_datetime(),
        Metadata::Track(track) => track.get(TrackInfoTag::CreateDate)?.as_datetime(),
    }
    .map(|dt| dt.into_naive())
}

fn mtime_date(path: &Path) -> Option<NaiveDateTime> {
    let mtime: chrono::DateTime<Local> = path.metadata().ok()?.modified().ok()?.into();
    Some(mtime.naive_local())
}

/// Render a Taken Date with a token format. Uppercase tokens only; every other
/// character is copied literally. `MM` means month before the first `HH` and
/// minute after it — that's what makes `YYYYMMDD_HHMMSS` unambiguous.
pub fn format_date(date: NaiveDateTime, fmt: &str) -> String {
    let mut out = String::new();
    let mut rest = fmt;
    let mut seen_time = false;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("YYYY") {
            out.push_str(&format!("{:04}", date.year()));
            rest = r;
        } else if let Some(r) = rest.strip_prefix("HH") {
            out.push_str(&format!("{:02}", date.hour()));
            seen_time = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("MM") {
            if seen_time {
                out.push_str(&format!("{:02}", date.minute()));
            } else {
                out.push_str(&format!("{:02}", date.month()));
            }
            rest = r;
        } else if let Some(r) = rest.strip_prefix("DD") {
            out.push_str(&format!("{:02}", date.day()));
            rest = r;
        } else if let Some(r) = rest.strip_prefix("SS") {
            out.push_str(&format!("{:02}", date.second()));
            rest = r;
        } else {
            let c = rest.chars().next().expect("rest is not empty");
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// Extensions worth importing — JPEG, HEIC, RAW (CR3/NEF/ARW/DNG), MP4/MOV.
/// Everything else on a card (`.THM`, sidecars, `MISC/`, `._` files) is
/// invisible to the importer.
const MEDIA_EXTENSIONS: [&str; 9] = [
    "jpg", "jpeg", "heic", "cr3", "nef", "arw", "dng", "mp4", "mov",
];

/// Standard Linux mount locations where removable media appears. Returns only
/// paths that exist — the scan step filters further.
pub fn detect_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(user) = std::env::var_os("USER") {
        for base in ["/run/media", "/media"] {
            let user_dir = PathBuf::from(base).join(&user);
            if user_dir.is_dir() {
                roots.push(user_dir);
            }
        }
    }
    let mnt = PathBuf::from("/mnt");
    if mnt.is_dir() {
        roots.push(mnt);
    }
    roots
}

/// Recursively find importable files under `roots`, sorted for a stable run.
/// Symlinked directories are not followed (no cycles off a mount).
pub fn scan(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        if root.is_file() {
            if is_media(root) && !is_junk(root) {
                found.push(root.clone());
            }
        } else {
            walk(root, &mut found);
        }
    }
    found.sort();
    found
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        // file_type() does not follow symlinks: symlinked dirs won't be
        // descended into, which is what keeps cycles off a mounted card.
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        if file_type.is_dir() {
            walk(&path, out);
        } else if file_type.is_file() && is_media(&path) && !is_junk(&path) {
            out.push(path);
        }
    }
}

fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.iter().any(|m| ext.eq_ignore_ascii_case(m)))
}

fn is_junk(path: &Path) -> bool {
    // AppleDouble resource files (`._IMG.jpg`) keep the media extension.
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("._"))
}

/// Scan `roots` and import everything found. The primary entry point.
pub fn import(roots: &[PathBuf], settings: &Settings) -> Report {
    import_files(&scan(roots), settings)
}

/// Copy every file into `<dest_root>/<DATE>/<DATE>_<name>`, creating date
/// folders as needed. The Source is never touched. One file's failure never
/// stops the rest of the run. Existing destination files are never
/// overwritten: identical content is skipped (re-runs are idempotent),
/// different content gets a `-1` suffix before the extension.
pub fn import_files(files: &[PathBuf], settings: &Settings) -> Report {
    let mut report = Report::default();
    for src in files {
        match import_one(src, settings) {
            Outcome::Imported(dest) => {
                report.mappings.push((src.clone(), dest));
                report.imported += 1;
            }
            Outcome::Skipped => report.skipped += 1,
            Outcome::Failed => report.failed += 1,
        }
    }
    report
}

enum Outcome {
    Imported(PathBuf),
    Skipped,
    Failed,
}

fn import_one(src: &Path, settings: &Settings) -> Outcome {
    let Some(date) = taken_date(src) else { return Outcome::Failed };
    let stamped = format_date(date, &settings.date_format);
    let Some(name) = src.file_name() else { return Outcome::Failed };
    let dir = settings.dest_root.join(&stamped);
    let primary = dir.join(format!("{stamped}_{}", name.to_string_lossy()));

    // 0 tries the plain name, then -1, -2, … A free slot means this content
    // isn't here yet; an occupied slot with our exact bytes means it already
    // is (idempotent re-run). Bound the hunt so a pathological card can't
    // spin forever.
    for n in 0..10_000u32 {
        let dest = if n == 0 {
            primary.clone()
        } else {
            suffixed(&primary, n)
        };
        if !dest.exists() {
            if std::fs::create_dir_all(&dir).is_err() {
                return Outcome::Failed;
            }
            return match std::fs::copy(src, &dest) {
                Ok(_) => Outcome::Imported(dest),
                Err(_) => Outcome::Failed,
            };
        }
        if same_content(src, &dest) {
            return Outcome::Skipped;
        }
    }
    Outcome::Failed
}

/// `name.jpg` → `name-1.jpg` (suffix before the extension, on the already
/// date-prefixed destination name).
fn suffixed(dest: &Path, n: u32) -> PathBuf {
    let dir = dest.parent().unwrap_or(Path::new(""));
    let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = dest.extension().and_then(|s| s.to_str()).unwrap_or("");
    dir.join(format!("{stem}-{n}.{ext}"))
}

fn same_content(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    // Size first: hashing only pays off for same-sized files.
    ma.len() == mb.len()
        && matches!(
            (content_hash(a), content_hash(b)),
            (Some(x), Some(y)) if x == y
        )
}

/// Streaming SipHash over the file's bytes. Same-process comparison only —
/// never persisted, so the unspecified-algorithm caveat doesn't matter.
fn content_hash(path: &Path) -> Option<u64> {
    use std::hash::Hasher;
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf).ok()?;
        if read == 0 {
            break;
        }
        hasher.write(&buf[..read]);
    }
    Some(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_formats() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 10)
            .unwrap()
            .and_hms_opt(14, 32, 55)
            .unwrap();
        assert_eq!(format_date(date, "YYYYMMDD"), "20261010");
        assert_eq!(format_date(date, "YYYY-MM-DD"), "2026-10-10");
        assert_eq!(format_date(date, "YYYYMMDD_HHMMSS"), "20261010_143255");
        // MM is month before HH, minute after it.
        assert_eq!(format_date(date, "MM/YYYY"), "10/2026");
        assert_eq!(format_date(date, "DD.MM.YYYY HH:MM"), "10.10.2026 14:32");
    }
}
