//! Import core: copies media files from a Source into a date-organized Destination.
//!
//! No UI, no gpui — the shell wraps this. Domain terms: see `GLOSSARY.md`.
//! Test seams live here: [`import_files`] and its per-file form
//! [`import_file`] (the entry points), and [`taken_date`] (the Taken Date
//! parser).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use nom_exif::{ExifTag, Metadata, TrackInfoTag, read_metadata};

pub mod usb;

/// Bounds of the parallel-imports stepper, and its fresh-install value.
pub const MIN_CONCURRENCY: usize = 1;
pub const MAX_CONCURRENCY: usize = 10;
pub const DEFAULT_CONCURRENCY: usize = 3;

/// Bring any concurrency value inside [`MIN_CONCURRENCY`]..=[`MAX_CONCURRENCY`].
/// Settings can arrive from a hand-edited file, the form or the stepper, so
/// every one of those paths clamps here rather than at the use site.
pub fn clamp_concurrency(n: usize) -> usize {
    n.clamp(MIN_CONCURRENCY, MAX_CONCURRENCY)
}

/// User-configured preferences that drive where and how files are written.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Root of the Destination; date folders are created under it as needed.
    pub dest_root: PathBuf,
    /// Token format (`YYYYMMDD`, `YYYY-MM-DD`, `YYYYMMDD_HHMMSS`) driving
    /// both the date folder name and the file-name prefix.
    pub date_format: String,
    /// How many files one Import may copy at once, always within
    /// [`MIN_CONCURRENCY`]..=[`MAX_CONCURRENCY`].
    pub concurrency: usize,
    /// Opt-in: keep USB devices powered during an Import (off by default).
    pub usb_power_fix: bool,
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
            concurrency: DEFAULT_CONCURRENCY,
            usb_power_fix: false,
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
/// shortcut: flat `key=value` lines, numbers parsed per key — enough while
/// settings stay flat; switch to a serialized format (toml + serde) when one
/// arrives that isn't a flat scalar.
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
            // A hand-edited file must not inject 0 or 500; anything that isn't
            // a number keeps the default.
            "concurrency" => {
                if let Ok(n) = value.parse::<usize>() {
                    settings.concurrency = clamp_concurrency(n);
                }
            }
            // Only an explicit `true` opts in; anything else (including a
            // typo) stays off, so the fix is never enabled by accident.
            "usb_power_fix" => settings.usb_power_fix = value.eq_ignore_ascii_case("true"),
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
            "dest_root={}\ndate_format={}\nconcurrency={}\nusb_power_fix={}\n",
            settings.dest_root.display(),
            settings.date_format,
            settings.concurrency,
            settings.usb_power_fix
        ),
    )
}

/// Build the [`Settings`] to persist from raw settings-form text plus the
/// stepper's value and the USB power fix toggle. Values are trimmed; a field
/// the user emptied keeps its current value — the flat file format can't
/// represent an empty value (see [`load_settings_from`]), and an empty
/// Destination root would scatter the next import into relative folders.
pub fn settings_from_form(
    dest_root: &str,
    date_format: &str,
    concurrency: usize,
    usb_power_fix: bool,
    current: &Settings,
) -> Settings {
    let dest_root = dest_root.trim();
    let date_format = date_format.trim();
    Settings {
        dest_root: if dest_root.is_empty() {
            current.dest_root.clone()
        } else {
            PathBuf::from(dest_root)
        },
        date_format: if date_format.is_empty() {
            current.date_format.clone()
        } else {
            date_format.to_string()
        },
        concurrency: clamp_concurrency(concurrency),
        usb_power_fix,
    }
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
/// Everything else on the Source (`.THM`, sidecars, `MISC/`, `._` files) is
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
/// Directories named `MISC` are not descended into (camera convention).
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
        // descended into, which is what keeps cycles off a mounted Source.
        let Ok(file_type) = entry.file_type() else { continue };
        let path = entry.path();
        if file_type.is_dir() {
            // MISC/ is a camera convention for thumbnails and miscellany, not
            // media — invisible at any depth (spec US12).
            if entry.file_name() == "MISC" {
                continue;
            }
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
///
/// Copies up to `settings.concurrency` files at once, with that many stdlib
/// scoped workers sharing a cursor over `files`. The outcomes (and therefore
/// [`Report::mappings`]) are always reported in `files` order, so for inputs
/// with distinct content a run at 2+ has the same order and counts as a
/// sequential one. At 1 this is today's loop.
///
/// Two *byte-identical* files landing in the same window are the exception to
/// the counts: the first reserves a name (empty) before its bytes land, so the
/// second cannot see identical content yet and claims a `-N` suffix instead of
/// reporting a skip. Accepted, and nothing is lost — both files are on disk at
/// distinct names. The alternative is holding the name lock across the copy,
/// which serializes exactly what this parallelizes.
pub fn import_files(files: &[PathBuf], settings: &Settings) -> Report {
    let workers = clamp_concurrency(settings.concurrency).min(files.len());
    let outcomes: Vec<Option<Outcome>> = if workers > 1 {
        let outcomes = Mutex::new(files.iter().map(|_| None).collect::<Vec<_>>());
        let cursor = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(src) = files.get(i) else { break };
                    let outcome = import_file(src, settings);
                    outcomes.lock().unwrap_or_else(|e| e.into_inner())[i] = Some(outcome);
                });
            }
        });
        outcomes.into_inner().unwrap_or_else(|e| e.into_inner())
    } else {
        // Empty list, or the user asked for 1: exactly the old loop.
        files.iter().map(|src| Some(import_file(src, settings))).collect()
    };
    let mut report = Report::default();
    for (src, outcome) in files.iter().zip(outcomes) {
        match outcome {
            Some(Outcome::Imported(dest)) => {
                report.mappings.push((src.clone(), dest));
                report.imported += 1;
            }
            Some(Outcome::Skipped(_)) => report.skipped += 1,
            // A worker visits every file, so `None` can't happen.
            Some(Outcome::Failed) | None => report.failed += 1,
        }
    }
    report
}

/// What one file did during an import.
#[derive(Debug)]
pub enum Outcome {
    Imported(PathBuf),
    /// Identical content already lives at this Destination path.
    Skipped(PathBuf),
    Failed,
}

/// Import a single file; what happened to it. [`import_files`] is this per
/// file, tallied into a [`Report`]. The shell calls this directly so each
/// row can show its own result as it lands.
pub fn import_file(src: &Path, settings: &Settings) -> Outcome {
    let Some(date) = taken_date(src) else { return Outcome::Failed };
    let stamped = format_date(date, &settings.date_format);
    let Some(name) = src.file_name() else { return Outcome::Failed };
    let dir = settings.dest_root.join(&stamped);
    let primary = dir.join(format!("{stamped}_{}", name.to_string_lossy()));

    let claim = {
        let _guard = NAME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        claim_name(&primary, src, &dir)
    };
    match claim {
        Claim::Reserved(dest) => match std::fs::copy(src, &dest) {
            Ok(_) => Outcome::Imported(dest),
            Err(_) => {
                // A failed copy must not leave its claim behind, or the next
                // file wanting this name would be pushed onto a suffix.
                let _ = std::fs::remove_file(&dest);
                Outcome::Failed
            }
        },
        Claim::Skipped(dest) => Outcome::Skipped(dest),
        Claim::Failed => Outcome::Failed,
    }
}

/// Serializes the Destination name hunt across every Import in this process.
/// Held only while probing a name (and comparing existing content), never
/// across a copy.
static NAME_LOCK: Mutex<()> = Mutex::new(());

enum Claim {
    /// The name is ours: the file was created exclusively (and is empty).
    Reserved(PathBuf),
    Skipped(PathBuf),
    Failed,
}

/// Find a free Destination name and create it exclusively so another file
/// cannot claim it too. 0 tries the plain name, then -1, -2, … An occupied
/// slot with our exact bytes means the content is already here (idempotent
/// re-run). Bound the hunt so a pathological Source can't spin forever.
fn claim_name(primary: &Path, src: &Path, dir: &Path) -> Claim {
    if std::fs::create_dir_all(dir).is_err() {
        return Claim::Failed;
    }
    for n in 0..10_000u32 {
        let dest = if n == 0 {
            primary.to_path_buf()
        } else {
            suffixed(primary, n)
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
        {
            Ok(_) => return Claim::Reserved(dest),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if same_content(src, &dest) {
                    return Claim::Skipped(dest);
                }
            }
            Err(_) => return Claim::Failed,
        }
    }
    Claim::Failed
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
