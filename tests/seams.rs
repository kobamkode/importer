//! Tests at the two agreed seams: `taken_date` (parser) and `import_files` (entry point).
//! Fixtures are real tiny files — a 1×1 JPEG and a 0.2s MP4 — with EXIF added
//! at test time via little_exif.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use kanistr::{
    DEFAULT_CONCURRENCY, MAX_CONCURRENCY, MIN_CONCURRENCY, Settings, format_date, import_files,
    load_settings_from, save_settings_to, taken_date,
};
use tempfile::TempDir;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

// HEIC/RAW fixtures untested: no encoder/sample on this machine — known gap, see importer-cb5 notes

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
        ..Settings::default()
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
    match kanistr::import_file(&src, &settings) {
        kanistr::Outcome::Skipped(dest) => assert_eq!(
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

    let found = kanistr::scan(&[a.path().to_path_buf(), b.path().to_path_buf()]);

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

    let found = kanistr::scan(&[root.path().to_path_buf()]);

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
        ..Settings::default()
    };

    let report = kanistr::import(&[root.path().to_path_buf()], &settings);

    assert_eq!(report.imported, 2);
    assert_eq!(report.failed, 0);
    assert_eq!(report.skipped, 0);

    // Second run: everything already there.
    let rerun = kanistr::import(&[root.path().to_path_buf()], &settings);
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

// ---------- parallel imports (importer-tk5.3) ----------

/// A Source tree with distinct names and Taken Dates: three JPEGs on
/// different days plus one video. No collisions, so a run at any concurrency
/// must land exactly the same files under the same names.
fn sample_source(dir: &Path) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir.join("DCIM/100CANON")).unwrap();
    let files = vec![
        jpeg_with_exif(dir, "IMG_0001.jpg", "2026:10:10 14:32:55"),
        jpeg_with_exif(dir, "IMG_0002.jpg", "2026:10:11 08:00:00"),
        jpeg_with_exif(dir, "IMG_0003.jpg", "2026:10:12 09:30:00"),
    ];
    let clip = dir.join("DCIM/100CANON/MVI_0004.mp4");
    std::fs::copy(fixture("clip.mp4"), &clip).unwrap();
    files.into_iter().chain([clip]).collect()
}

#[test]
fn concurrency_1_and_3_produce_the_same_import() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().join("source");
    let files = sample_source(&source);

    let run = |concurrency: usize, root: &str| {
        let settings = Settings {
            dest_root: dir.path().join(root),
            date_format: "YYYYMMDD".into(),
            concurrency,
            ..Settings::default()
        };
        let report = import_files(&files, &settings);
        (settings.dest_root, report)
    };
    let (one_root, one) = run(1, "dest-one");
    let (three_root, three) = run(3, "dest-three");

    assert_eq!(one.imported, files.len(), "every file lands one at a time");
    assert_eq!(
        (three.imported, three.failed, three.skipped),
        (one.imported, one.failed, one.skipped),
        "the same inputs report the same counts at 1 and 3"
    );

    // Destinations as paths relative to their root, so the two runs compare.
    let destinations = |report: &kanistr::Report, root: &Path| -> Vec<(String, String)> {
        report
            .mappings
            .iter()
            .map(|(src, dest)| {
                (
                    src.strip_prefix(&source).unwrap().display().to_string(),
                    dest.strip_prefix(root).unwrap().display().to_string(),
                )
            })
            .collect()
    };
    let one_map = destinations(&one, &one_root);
    let three_map = destinations(&three, &three_root);
    assert_eq!(
        one_map, three_map,
        "same Sources map to the same Destination paths at 1 and 3"
    );
    for (_, dest) in &one_map {
        assert_eq!(
            std::fs::read(one_root.join(dest)).unwrap(),
            std::fs::read(three_root.join(dest)).unwrap(),
            "Destination bytes must not depend on concurrency: {dest}"
        );
    }
}

#[test]
fn colliding_names_both_land_at_concurrency_3() {
    let dir = TempDir::new().unwrap();
    // Two different files, same file name, same Taken Date → same Destination
    // name. At 3 they are claimed in the same window, which is the contention
    // the reservation lock must survive: both land, neither clobbered.
    let jpeg = jpeg_with_exif(dir.path(), "IMG_1.jpg", "2026:10:10 14:32:55");
    let video = dir.path().join("other").join("IMG_1.jpg");
    std::fs::create_dir_all(video.parent().unwrap()).unwrap();
    std::fs::copy(fixture("clip.mp4"), &video).unwrap();
    let settings = Settings {
        concurrency: 3,
        ..settings_in(&dir, "YYYYMMDD")
    };

    let report = import_files(&[jpeg.clone(), video.clone()], &settings);

    assert_eq!(report.imported, 2, "both land, neither overwritten");
    assert_eq!(report.failed, 0);
    let date_dir = settings.dest_root.join("20261010");
    let mut names: Vec<String> = std::fs::read_dir(&date_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["20261010_IMG_1-1.jpg", "20261010_IMG_1.jpg"],
        "one plain name and one suffix, nothing else"
    );
    let mut landed: Vec<Vec<u8>> = names
        .iter()
        .map(|name| std::fs::read(date_dir.join(name)).unwrap())
        .collect();
    let mut sources = vec![
        std::fs::read(&jpeg).unwrap(),
        std::fs::read(&video).unwrap(),
    ];
    landed.sort();
    sources.sort();
    assert_eq!(landed, sources, "both files land byte-for-byte intact");
}

// ---------- failed copy cleanup (importer-tk5.2) ----------

/// A Source that yields a Taken Date (its mtime) but that `std::fs::copy`
/// cannot read: a directory named like a photo. It fails *after* the
/// Destination name is claimed, which is the path the cleanup must survive.
fn uncopyable_source(dir: &Path, file_name: &str) -> PathBuf {
    let path = dir.join(file_name);
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn failed_copy_leaves_no_file_at_claimed_destination() {
    let dir = TempDir::new().unwrap();
    let broken = uncopyable_source(dir.path(), "IMG_1.jpg");
    let settings = settings_in(&dir, "YYYYMMDD");
    let stamped = format_date(taken_date(&broken).unwrap(), &settings.date_format);

    let report = import_files(&[broken], &settings);

    assert_eq!(report.failed, 1);
    let claimed = settings
        .dest_root
        .join(&stamped)
        .join(format!("{stamped}_IMG_1.jpg"));
    assert!(
        !claimed.exists(),
        "failed copy must remove its claimed name: {}",
        claimed.display()
    );
}

#[test]
fn file_after_failed_copy_gets_plain_name() {
    let dir = TempDir::new().unwrap();
    let broken = uncopyable_source(dir.path(), "IMG_1.jpg");
    // Same file name and Taken Date as `broken`, but this one can be copied.
    let good = dir.path().join("other").join("IMG_1.jpg");
    std::fs::create_dir_all(good.parent().unwrap()).unwrap();
    std::fs::copy(fixture("base.jpg"), &good).unwrap();
    // The directory's mtime is what `taken_date` reads for the broken Source;
    // copy it onto the good file instead of writing a directory's mtime.
    let mtime = std::fs::metadata(&broken).unwrap().modified().unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&good)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    // Sequential on purpose: "after a failed copy" is a guarantee about the
    // order files are launched in. In one parallel window the good file may
    // claim its name while the broken one still holds the plain name.
    let mut settings = settings_in(&dir, "YYYYMMDD");
    settings.concurrency = 1;
    let stamped = format_date(taken_date(&broken).unwrap(), &settings.date_format);

    let report = import_files(&[broken, good.clone()], &settings);

    assert_eq!(report.failed, 1);
    assert_eq!(report.imported, 1);
    let plain = settings
        .dest_root
        .join(&stamped)
        .join(format!("{stamped}_IMG_1.jpg"));
    assert_eq!(
        std::fs::read(&plain).unwrap(),
        std::fs::read(&good).unwrap(),
        "the good file must land under the plain name"
    );
    assert!(
        !settings
            .dest_root
            .join(&stamped)
            .join(format!("{stamped}_IMG_1-1.jpg"))
            .exists(),
        "a failed copy must not push the next file onto a suffix"
    );
}

// ---------- settings persistence (importer-cb5.4) ----------

#[test]
fn settings_round_trip() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("kanistr/settings.conf");
    let settings = Settings {
        dest_root: dir.path().join("somewhere/photos"),
        date_format: "YYYY-MM-DD".into(),
        concurrency: 7,
        usb_power_fix: true,
    };

    save_settings_to(&path, &settings).unwrap();
    let loaded = load_settings_from(&path);

    assert_eq!(loaded, settings);
    // Each setting is its own flat key, not a format change.
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("concurrency=7"), "saved file: {text}");
    assert!(text.contains("usb_power_fix=true"), "saved file: {text}");

    // And off survives a save/load too: the toggle must stay off once turned
    // off, not just default to off.
    let off = Settings {
        usb_power_fix: false,
        ..settings
    };
    save_settings_to(&path, &off).unwrap();
    assert_eq!(load_settings_from(&path), off);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("usb_power_fix=false"), "saved file: {text}");
}

#[test]
fn fresh_settings_have_usb_power_fix_off() {
    assert!(
        !Settings::default().usb_power_fix,
        "the fix must be opt-in: a fresh install never touches USB power"
    );
}

#[test]
fn file_without_usb_power_fix_loads_off_and_keeps_other_settings() {
    // A settings file written before the toggle existed.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.conf");
    std::fs::write(
        &path,
        "dest_root=/cards/imports\ndate_format=YYYY-MM-DD\nconcurrency=5\n",
    )
    .unwrap();

    let loaded = load_settings_from(&path);

    assert!(!loaded.usb_power_fix);
    assert_eq!(loaded.dest_root, PathBuf::from("/cards/imports"));
    assert_eq!(loaded.date_format, "YYYY-MM-DD");
    assert_eq!(loaded.concurrency, 5);
}

#[test]
fn unparseable_usb_power_fix_loads_off() {
    // A hand-edited file must not turn the fix on by accident.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.conf");
    std::fs::write(&path, "usb_power_fix=yes please\n").unwrap();

    assert!(!load_settings_from(&path).usb_power_fix);
}

#[test]
fn file_without_concurrency_loads_3_and_keeps_other_settings() {
    // A settings file written before the setting existed.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("settings.conf");
    std::fs::write(&path, "dest_root=/cards/imports\ndate_format=YYYY-MM-DD\n").unwrap();

    let loaded = load_settings_from(&path);

    assert_eq!(loaded.concurrency, 3);
    assert_eq!(loaded.dest_root, PathBuf::from("/cards/imports"));
    assert_eq!(loaded.date_format, "YYYY-MM-DD");
}

#[test]
fn out_of_range_or_non_numeric_concurrency_is_clamped_on_load() {
    let dir = TempDir::new().unwrap();

    for (written, expected) in [
        ("0", MIN_CONCURRENCY),
        ("11", MAX_CONCURRENCY),
        ("500", MAX_CONCURRENCY),
        ("many", DEFAULT_CONCURRENCY),
        ("-2", DEFAULT_CONCURRENCY),
    ] {
        let path = dir.path().join(format!("{written}.conf"));
        std::fs::write(&path, format!("concurrency={written}\n")).unwrap();
        assert_eq!(
            load_settings_from(&path).concurrency,
            expected,
            "concurrency={written} must land inside 1–10"
        );
    }
}

#[test]
fn missing_settings_file_yields_defaults_through_entry_point() {
    let dir = TempDir::new().unwrap();
    let mut settings = load_settings_from(&dir.path().join("nope.conf"));
    assert_eq!(settings, Settings::default());
    assert_eq!(settings.date_format, "YYYYMMDD");
    assert_eq!(settings.concurrency, 3, "fresh install defaults to 3 files at once");
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
        ..Settings::default()
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
    assert_eq!(loaded.concurrency, 3);
}

// ---------- settings form input (importer-cb5.6) ----------

#[test]
fn form_text_builds_settings_with_empty_fallback() {
    let current = Settings {
        dest_root: PathBuf::from("/photos/imports"),
        date_format: "YYYYMMDD".into(),
        concurrency: 3,
        usb_power_fix: true,
    };

    // Non-empty fields are trimmed and used; the stepper value and the USB
    // toggle ride along, so the form keeps one path into Settings.
    assert_eq!(
        kanistr::settings_from_form("  /new/dest \n", " YYYY-MM-DD ", 7, true, &current),
        Settings {
            dest_root: PathBuf::from("/new/dest"),
            date_format: "YYYY-MM-DD".into(),
            concurrency: 7,
            usb_power_fix: true,
        }
    );
    assert_eq!(
        kanistr::settings_from_form("/new/dest", "YYYY-MM-DD", 7, false, &current),
        Settings {
            dest_root: PathBuf::from("/new/dest"),
            date_format: "YYYY-MM-DD".into(),
            concurrency: 7,
            usb_power_fix: false,
        }
    );

    // An emptied field keeps its current value: the flat file can't hold an
    // empty value, and an empty Destination root would scatter the next
    // import into relative folders.
    assert_eq!(
        kanistr::settings_from_form("", "", current.concurrency, true, &current),
        current
    );
    assert_eq!(
        kanistr::settings_from_form("", "YYYYMMDD_HHMMSS", 1, false, &current),
        Settings {
            dest_root: current.dest_root.clone(),
            date_format: "YYYYMMDD_HHMMSS".into(),
            concurrency: 1,
            usb_power_fix: false,
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
