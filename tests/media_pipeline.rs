mod support;

use globustut_uploader_to_commons::db::Database;
use globustut_uploader_to_commons::media::{FIXED_UPLOAD_LICENSE, MediaProcessor};
use std::sync::Arc;

#[tokio::test]
async fn preserves_supported_jpeg_bytes_and_extracts_exif_gps() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db.clone());

    let source = config.temp_dir.join("source.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "64x48", "xc:#4cb6ac"])
            .arg(&source),
    );
    support::run(
        std::process::Command::new("exiftool")
            .args([
                "-overwrite_original",
                "-GPSLatitude=53.9045",
                "-GPSLatitudeRef=N",
                "-GPSLongitude=27.5615",
                "-GPSLongitudeRef=E",
                "-DateTimeOriginal=2025:07:18 12:13:14",
            ])
            .arg(&source),
    );
    let original = std::fs::read(&source).unwrap();

    let upload = processor
        .ingest(&source, "Minsk photo.JPG", "Category:Buildings_in_Minsk")
        .await
        .unwrap();

    assert!(!upload.converted);
    assert_eq!(upload.display_filename, "Minsk photo.jpg");
    assert_eq!(upload.commons_category, "Buildings in Minsk");
    assert_eq!(upload.license, FIXED_UPLOAD_LICENSE);
    assert_eq!(upload.capture_date.as_deref(), Some("2025-07-18"));
    assert!((upload.latitude.unwrap() - 53.9045).abs() < 0.0001);
    assert!((upload.longitude.unwrap() - 27.5615).abs() < 0.0001);
    assert_eq!(
        std::fs::read(processor.stored_path(&upload).unwrap()).unwrap(),
        original
    );
    assert!(processor.thumbnail_path(&upload.id).is_file());
}

#[tokio::test]
async fn converts_bmp_to_commons_compatible_webp() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);

    let source = config.temp_dir.join("source.bmp");
    support::run(
        support::imagemagick()
            .args(["-size", "31x19", "xc:#d43d2f"])
            .arg(&source),
    );
    let upload = processor
        .ingest(&source, "camera.BMP", "Belarus")
        .await
        .unwrap();

    assert!(upload.converted);
    assert_eq!(upload.mime_type, "image/webp");
    assert_eq!(upload.display_filename, "camera.webp");
    let bytes = std::fs::read(processor.stored_path(&upload).unwrap()).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WEBP");
}

#[tokio::test]
async fn rejects_non_media_content() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);
    let source = config.temp_dir.join("fake.jpg");
    std::fs::write(&source, b"not an image or video").unwrap();

    let error = processor
        .ingest(&source, "fake.jpg", "Belarus")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("unsupported or invalid"));
}

#[tokio::test]
async fn startup_cleanup_removes_orphans_but_preserves_indexed_media() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);

    let source = config.temp_dir.join("source.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "20x20", "xc:#4cb6ac"])
            .arg(&source),
    );
    let upload = processor
        .ingest(&source, "kept.jpg", "Belarus")
        .await
        .unwrap();
    let kept_media = processor.stored_path(&upload).unwrap();
    let kept_thumbnail = processor.thumbnail_path(&upload.id);

    let orphan_media = config.upload_dir.join("orphan.part");
    let orphan_thumbnail = config.thumbnail_dir.join("orphan.webp");
    let stale_buffer = config
        .temp_dir
        .join(format!("upload-{}", uuid::Uuid::new_v4()));
    let unrelated_temp = config.temp_dir.join("operator-note.txt");
    for path in [
        &orphan_media,
        &orphan_thumbnail,
        &stale_buffer,
        &unrelated_temp,
    ] {
        std::fs::write(path, b"fixture").unwrap();
    }

    assert_eq!(processor.cleanup_orphaned_files().await.unwrap(), 3);
    assert!(kept_media.is_file());
    assert!(kept_thumbnail.is_file());
    assert!(!orphan_media.exists());
    assert!(!orphan_thumbnail.exists());
    assert!(!stale_buffer.exists());
    assert!(unrelated_temp.is_file());
}
