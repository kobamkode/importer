//! Tests at the two agreed seams: `taken_date` (parser) and `import_files` (entry point).
//! Fixtures are real tiny files — a 1×1 JPEG and a 0.2s MP4 — with EXIF added
//! at test time via little_exif.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use importer::{Settings, format_date, import_files, load_settings_from, save_settings_to, taken_date};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Copy a fixture into `dir` and stamp it with a known DateTimeOriginal.
fn jpeg_with_exif(dir: &Path, file_name: &str, exif_date: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::copy(fixture("base.jpg"), &path).unwrap();
    let mut md = little_exif::metadata::Metadata::new();
    md.set_tag(little_exif::exif_tag::ExifTag::DateTimeOriginal(exif_date.into()));
    md.write_to_file(&path).unwrap();
    path
}

// ---------- seam 2: taken_date ----------

#[test]
fn taken_date_reads_exif_datetime_original() {
    let dir = TempDir::new().unwrap();
    let jpg = jpeg_with_exif(dir.path(), "shot.jpg", "2026:10:10 14:32:55");
    let date = taken_date(&jpg).expect("EXIF date should parse");
    assert_eq!(date.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-10-10 14:32:55");
}

#[test]
fn taken_date_falls_back_to_mtime_without_exif() {
    let dir = TempDir::new().unwrap();
    let jpg = dir.path().join("plain.jpg");
    std::fs::copy(fixture("base.jpg"), &jpg).unwrap();
    // Set a known mtime: 2026-01-02 03:04:05 UTC.
    let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(1767323045);
    let f = std::fs::OpenOptions::new().write(true).open(&jpg).unwrap();
    f.set_modified(mtime).unwrap();

    let date = taken_date(&jpg).expect("mtime fallback should yield a date");
    // Local time of that instant; compare via the same conversion the code uses.
    let expected: chrono::DateTime<chrono::Local> = mtime.into();
    assert_eq!(date, expected.naive_local());
}

#[test]
fn taken_date_reads_video_creation_time() {
    let date = taken_date(&fixture("clip.mp4")).expect("video CreateDate should parse");
    assert_eq!(date.format("%Y-%m-%d %H:%M:%S").to_string(), "2026-10-10 14:32:55");
}

#[test]
fn taken_date_unreadable_file_is_none() {
    assert!(taken_date(Path::new("/nonexistent/nope.jpg")).is_none());
}

// ---------- seam 1: import_files ----------

fn settings_in(dir: &TempDir, fmt: &str) -> Settings {
    Settings {
        dest_root: dir.path().join("dest"),
        date_format: fmt.to_string(),
    }
}

#[test]
fn imports_into_date_folder_with_date_prefix() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "IMG_1234.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYYMMDD");

    let report = import_files(&[src.clone()], &settings);

    assert_eq!(report.imported, 1);
    assert_eq!(report.failed, 0);
    let dest = settings.dest_root.join("20261010/20261010_IMG_1234.jpg");
    assert!(dest.exists(), "dest should exist: {}", dest.display());
    assert_eq!(report.mappings, vec![(src.clone(), dest.clone())]);
    // Source untouched, contents identical.
    assert!(src.exists());
    assert_eq!(
        std::fs::read(&src).unwrap(),
        std::fs::read(&dest).unwrap()
    );
}

#[test]
fn date_format_drives_folder_and_prefix() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "a.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYY-MM-DD");

    import_files(&[src], &settings);

    assert!(settings.dest_root.join("2026-10-10/2026-10-10_a.jpg").exists());
}

#[test]
fn creates_date_folders_automatically() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "b.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYYMMDD");

    assert!(!settings.dest_root.exists());
    import_files(&[src], &settings);

    assert!(settings.dest_root.join("20261010").is_dir());
}

#[test]
fn corrupt_file_fails_counted_run_continues() {
    let dir = TempDir::new().unwrap();
    // A missing file fails at metadata AND mtime, so it must be counted as
    // failed — and must not stop the good file behind it.
    let good = jpeg_with_exif(dir.path(), "good.jpg", "2026:10:10 14:32:55");
    let missing = dir.path().join("gone.jpg"); // does not exist
    let settings = settings_in(&dir, "YYYYMMDD");

    let report = import_files(&[missing, good], &settings);

    assert_eq!(report.failed, 1);
    assert_eq!(report.imported, 1, "failure must not stop the run");
}

#[test]
fn existing_destination_file_is_not_overwritten() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "IMG_1.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYYMMDD");
    let dest_dir = settings.dest_root.join("20261010");
    std::fs::create_dir_all(&dest_dir).unwrap();
    std::fs::write(dest_dir.join("20261010_IMG_1.jpg"), b"sentinel").unwrap();

    let report = import_files(&[src], &settings);

    // Different content at the name → suffixed copy, sentinel untouched.
    assert_eq!(report.imported, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(
        std::fs::read(dest_dir.join("20261010_IMG_1.jpg")).unwrap(),
        b"sentinel"
    );
    assert!(dest_dir.join("20261010_IMG_1-1.jpg").exists());
}

#[test]
fn skipped_file_reports_existing_destination() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "IMG_9.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYYMMDD");
    import_files(&[src.clone()], &settings);

    // US13: the user must be able to verify where every file went — skipped
    // files report the Destination path that already holds identical content.
    match importer::import_file(&src, &settings) {
        importer::Outcome::Skipped(dest) => assert_eq!(
            dest,
            settings.dest_root.join("20261010/20261010_IMG_9.jpg")
        ),
        other => panic!("expected Skipped(existing destination), got {other:?}"),
    }
}

#[test]
fn identical_content_is_skipped_on_rerun() {
    let dir = TempDir::new().unwrap();
    let src = jpeg_with_exif(dir.path(), "IMG_9.jpg", "2026:10:10 14:32:55");
    let settings = settings_in(&dir, "YYYYMMDD");

    let first = import_files(&[src.clone()], &settings);
    let second = import_files(&[src], &settings);

    assert_eq!(first.imported, 1);
    assert_eq!(second.imported, 0);
    assert_eq!(second.skipped, 1, "identical content is already there");
    // No `-1` litter.
    assert!(!settings.dest_root.join("20261010/20261010_IMG_9-1.jpg").exists());
}

// ---------- seam 1: import (scan + import) ----------

#[test]
fn scan_finds_media_recursively_across_roots_and_skips_junk() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    // Root A: camera-style layout with junk.
    std::fs::create_dir_all(a.path().join("DCIM/100CANON")).unwrap();
    std::fs::create_dir_all(a.path().join("MISC")).unwrap();
    std::fs::copy(fixture("base.jpg"), a.path().join("DCIM/100CANON/IMG_0001.JPG")).unwrap();
    std::fs::copy(fixture("base.jpg"), a.path().join("DCIM/IMG_0002.heic")).unwrap();
    std::fs::copy(fixture("clip.mp4"), a.path().join("DCIM/MVI_0003.mp4")).unwrap();
    std::fs::write(a.path().join("MISC/THM.THM"), b"x").unwrap(); // not a media ext... .THM is not listed
    std::fs::write(a.path().join("DCIM/note.txt"), b"x").unwrap();
    std::fs::write(a.path().join("DCIM/._IMG_hidden.jpg"), b"x").unwrap(); // AppleDouble junk
    // Root B: second mount.
    std::fs::create_dir_all(b.path().join("DCIM")).unwrap();
    std::fs::copy(fixture("base.jpg"), b.path().join("DCIM/IMG_1000.jpg")).unwrap();

    let found = importer::scan(&[a.path().to_path_buf(), b.path().to_path_buf()]);

    let mut names: Vec<String> = found
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    // Two roots are random temp dirs; order across them isn't meaningful here.
    names.sort();
    assert_eq!(names, vec!["IMG_0001.JPG", "IMG_0002.heic", "IMG_1000.jpg", "MVI_0003.mp4"]);
}

#[test]
fn misc_directory_is_invisible() {
    let root = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("DCIM")).unwrap();
    std::fs::create_dir_all(root.path().join("MISC")).unwrap();
    std::fs::copy(fixture("base.jpg"), root.path().join("MISC/whatever.jpg")).unwrap();
    std::fs::copy(fixture("base.jpg"), root.path().join("DCIM/keep.jpg")).unwrap();

    let found = importer::scan(&[root.path().to_path_buf()]);

    assert_eq!(
        found,
        vec![root.path().join("DCIM/keep.jpg")],
        "MISC/ holds non-media per camera convention (US12); DCIM/ is scanned"
    );
}

#[test]
fn import_from_roots_produces_full_counts() {
    let root = TempDir::new().unwrap();
    let dest = TempDir::new().unwrap();
    std::fs::create_dir_all(root.path().join("DCIM/100CANON")).unwrap();
    std::fs::copy(fixture("base.jpg"), root.path().join("DCIM/100CANON/a.jpg")).unwrap();
    std::fs::copy(fixture("clip.mp4"), root.path().join("DCIM/100CANON/b.mp4")).unwrap();
    std::fs::write(root.path().join("DCIM/notes.txt"), b"x").unwrap();
    let settings = Settings {
        dest_root: dest.path().to_path_buf(),
        date_format: "YYYYMMDD".into(),
    };

    let report = importer::import(&[root.path().to_path_buf()], &settings);

    assert_eq!(report.imported, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(report.skipped, 0);

    // Second run: everything already there.
    let rerun = importer::import(&[root.path().to_path_buf()], &settings);
    assert_eq!(rerun.imported, 0);
    assert_eq!(rerun.skipped, 2, "idempotent re-run");
}

#[test]
fn same_name_different_content_gets_suffix() {
    let dir = TempDir::new().unwrap();
    // Two different files, same name, same Taken Date → same dest name.
    let one = jpeg_with_exif(dir.path(), "IMG_1.jpg", "2026:10:10 14:32:55");
    let two = dir.path().join("other").join("IMG_1.jpg");
    std::fs::create_dir_all(two.parent().unwrap()).unwrap();
    std::fs::copy(fixture("clip.mp4"), &two).unwrap(); // different bytes, same name
    let settings = settings_in(&dir, "YYYYMMDD");

    let report = import_files(&[one, two], &settings);

    assert_eq!(report.imported, 2, "both land, neither clobbered");
    let d = settings.dest_root.join("20261010");
    assert!(d.join("20261010_IMG_1.jpg").exists());
    assert!(d.join("20261010_IMG_1-1.jpg").exists());
}

// ---------- settings persistence (importer-cb5.4) ----------

#[test]
fn settings_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("importer/settings.conf");
    let settings = Settings {
        dest_root: dir.path().join("somewhere/photos"),
        date_format: "YYYY-MM-DD".into(),
    };

    save_settings_to(&path, &settings).unwrap();
    let loaded = load_settings_from(&path);

    assert_eq!(loaded, settings);
}

#[test]
fn missing_settings_file_yields_defaults_through_entry_point() {
    let dir = TempDir::new().unwrap();
    let mut settings = load_settings_from(&dir.path().join("nope.conf"));
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.date_format, "YYYYMMDD");
    assert!(
        settings.dest_root.ends_with("Pictures/Imports"),
        "default dest is under HOME: {}",
        settings.dest_root.display()
    );

    // Assert through the entry point with the loaded default date format;
    // only the root is redirected so the test never writes into real $HOME.
    settings.dest_root = dir.path().join("dest");
    let src = jpeg_with_exif(dir.path(), "dflt.jpg", "2026:10:10 14:32:55");
    import_files(&[src], &settings);
    assert!(settings.dest_root.join("20261010/20261010_dflt.jpg").exists());
}

#[test]
fn changing_date_format_changes_next_import() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.conf");
    let mut settings = Settings {
        dest_root: dir.path().join("dest"),
        date_format: "YYYY-MM-DD".into(),
    };
    save_settings_to(&path, &settings).unwrap();

    // User switches the format in settings between runs.
    settings.date_format = "YYYYMMDD_HHMMSS".into();
    save_settings_to(&path, &settings).unwrap();

    let loaded = load_settings_from(&path);
    let src = jpeg_with_exif(dir.path(), "fmt.jpg", "2026:10:10 14:32:55");
    import_files(&[src], &loaded);

    let folder = "20261010_143255";
    assert!(
        loaded.dest_root.join(folder).join(format!("{folder}_fmt.jpg")).exists(),
        "folder and prefix must both use the loaded format"
    );
}

#[test]
fn settings_file_is_the_single_mechanism() {
    // Structural, not env-sensitive: the loader reads only the file at the
    // given path — HOME/XDG_CONFIG_HOME are consulted for *locating* the file
    // (settings_path), never for overriding its values. An empty or garbage
    // file still yields defaults, not env values.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.conf");
    std::fs::write(&path, "garbage line without separator\ndate_format=\nunknown_key=x\n").unwrap();

    let loaded = load_settings_from(&path);

    // Empty value rejected at the boundary → default kept.
    assert_eq!(loaded.date_format, "YYYYMMDD");
    assert_eq!(loaded.dest_root, Settings::default().dest_root);
}

// ---------- settings form input (importer-cb5.6) ----------

#[test]
fn form_text_builds_settings_with_empty_fallback() {
    let current = Settings {
        dest_root: PathBuf::from("/photos/imports"),
        date_format: "YYYYMMDD".into(),
    };

    // Non-empty fields are trimmed and used.
    assert_eq!(
        importer::settings_from_form("  /new/dest \n", " YYYY-MM-DD ", &current),
        Settings {
            dest_root: PathBuf::from("/new/dest"),
            date_format: "YYYY-MM-DD".into(),
        }
    );

    // An emptied field keeps its current value: the flat file can't hold an
    // empty value, and an empty Destination root would scatter the next
    // import into relative folders.
    assert_eq!(
        importer::settings_from_form("", "", &current),
        current
    );
    assert_eq!(
        importer::settings_from_form("", "YYYYMMDD_HHMMSS", &current),
        Settings {
            dest_root: current.dest_root.clone(),
            date_format: "YYYYMMDD_HHMMSS".into(),
        }
    );
}

// ---------- format_date, as used by the entry point ----------

#[test]
fn format_matches_settings_convention() {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 10)
        .unwrap()
        .and_hms_opt(14, 32, 55)
        .unwrap();
    assert_eq!(format_date(date, "YYYYMMDD_HHMMSS"), "20261010_143255");
}
