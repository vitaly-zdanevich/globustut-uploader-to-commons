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
                "-EXIF:Artist=Fallback EXIF Artist",
                "-XMP-dc:Creator=Ada Lovelace",
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
    assert_eq!(upload.author.as_deref(), Some("Ada Lovelace"));
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
async fn extracts_embedded_jpeg_from_dng_before_conversion() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);

    let preview = config.temp_dir.join("preview.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "48x32", "xc:#4cb6ac"])
            .arg(&preview),
    );
    let preview_bytes = std::fs::read(&preview).unwrap();
    let source = config.temp_dir.join("dng-upload");
    write_dng_with_preview(&source, &preview_bytes, 48, 32);

    let upload = processor
        .ingest(&source, "camera.DNG", "Belarus")
        .await
        .unwrap();

    assert!(upload.converted);
    assert_eq!(upload.mime_type, "image/jpeg");
    assert_eq!(upload.display_filename, "camera.jpg");
    assert_eq!(
        std::fs::read(processor.stored_path(&upload).unwrap()).unwrap(),
        preview_bytes
    );
}

#[tokio::test]
async fn converts_dng_to_webp_when_it_has_no_embedded_jpeg() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);

    let source = config.temp_dir.join("dng-upload");
    write_raw_dng(&source, 32, 24);

    let upload = processor
        .ingest(&source, "camera.dng", "Belarus")
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
async fn does_not_treat_an_ordinary_tiff_named_dng_as_raw() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let processor = MediaProcessor::new(config.clone(), db);

    let source = config.temp_dir.join("ordinary-tiff");
    support::run(
        support::imagemagick()
            .args(["-size", "29x17", "xc:#4cb6ac"])
            .arg(format!("TIFF:{}", source.display())),
    );
    let original = std::fs::read(&source).unwrap();
    let upload = processor
        .ingest(&source, "misnamed.dng", "Belarus")
        .await
        .unwrap();

    assert!(!upload.converted);
    assert_eq!(upload.mime_type, "image/tiff");
    assert_eq!(upload.display_filename, "misnamed.tif");
    assert_eq!(
        std::fs::read(processor.stored_path(&upload).unwrap()).unwrap(),
        original
    );
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
    let stale_dng_jpeg = config
        .temp_dir
        .join(format!("{}.jpg", uuid::Uuid::new_v4()));
    let stale_raw_development = config
        .temp_dir
        .join(format!("{}.raw.ppm", uuid::Uuid::new_v4()));
    let stale_dng_preview = config
        .temp_dir
        .join(format!("{}.dng-preview.jpg", uuid::Uuid::new_v4()));
    let unrelated_temp = config.temp_dir.join("operator-note.txt");
    for path in [
        &orphan_media,
        &orphan_thumbnail,
        &stale_buffer,
        &stale_dng_jpeg,
        &stale_raw_development,
        &stale_dng_preview,
        &unrelated_temp,
    ] {
        std::fs::write(path, b"fixture").unwrap();
    }

    assert_eq!(processor.cleanup_orphaned_files().await.unwrap(), 6);
    assert!(kept_media.is_file());
    assert!(kept_thumbnail.is_file());
    assert!(!orphan_media.exists());
    assert!(!orphan_thumbnail.exists());
    assert!(!stale_buffer.exists());
    assert!(!stale_dng_jpeg.exists());
    assert!(!stale_raw_development.exists());
    assert!(!stale_dng_preview.exists());
    assert!(unrelated_temp.is_file());
}

fn write_dng_with_preview(path: &std::path::Path, jpeg: &[u8], width: u32, height: u32) {
    const ENTRY_COUNT: u16 = 11;
    let bits_offset = 8 + 2 + u32::from(ENTRY_COUNT) * 12 + 4;
    let jpeg_offset = bits_offset + 6;
    let jpeg_len = u32::try_from(jpeg.len()).unwrap();
    let mut dng = Vec::with_capacity(usize::try_from(jpeg_offset + jpeg_len).unwrap());
    dng.extend_from_slice(b"II");
    dng.extend_from_slice(&42_u16.to_le_bytes());
    dng.extend_from_slice(&8_u32.to_le_bytes());
    dng.extend_from_slice(&ENTRY_COUNT.to_le_bytes());
    push_tiff_long(&mut dng, 254, 1);
    push_tiff_long(&mut dng, 256, width);
    push_tiff_long(&mut dng, 257, height);
    push_tiff_entry(&mut dng, 258, 3, 3, bits_offset);
    push_tiff_short(&mut dng, 259, 7);
    push_tiff_short(&mut dng, 262, 6);
    push_tiff_long(&mut dng, 273, jpeg_offset);
    push_tiff_short(&mut dng, 277, 3);
    push_tiff_long(&mut dng, 278, height);
    push_tiff_long(&mut dng, 279, jpeg_len);
    push_tiff_entry(&mut dng, 50_706, 1, 4, u32::from_le_bytes([1, 4, 0, 0]));
    dng.extend_from_slice(&0_u32.to_le_bytes());
    for bits in [8_u16, 8, 8] {
        dng.extend_from_slice(&bits.to_le_bytes());
    }
    dng.extend_from_slice(jpeg);
    std::fs::write(path, dng).unwrap();
}

fn write_raw_dng(path: &std::path::Path, width: u32, height: u32) {
    // Minimal uncompressed RGGB DNG with the required camera/color metadata.
    const ENTRY_COUNT: u16 = 20;
    const MAKE_OFFSET: u32 = 254;
    const MODEL_OFFSET: u32 = 268;
    const UNIQUE_MODEL_OFFSET: u32 = 282;
    const COLOR_MATRIX_OFFSET: u32 = 296;
    const NEUTRAL_OFFSET: u32 = 368;
    const RAW_OFFSET: u32 = 392;
    const CAMERA: &[u8; 13] = b"SyntheticCam\0";

    let raw_len = width.checked_mul(height).unwrap().checked_mul(2).unwrap();
    let mut dng = Vec::with_capacity(usize::try_from(RAW_OFFSET + raw_len).unwrap());
    dng.extend_from_slice(b"II");
    dng.extend_from_slice(&42_u16.to_le_bytes());
    dng.extend_from_slice(&8_u32.to_le_bytes());
    dng.extend_from_slice(&ENTRY_COUNT.to_le_bytes());
    push_tiff_long(&mut dng, 254, 0);
    push_tiff_long(&mut dng, 256, width);
    push_tiff_long(&mut dng, 257, height);
    push_tiff_short(&mut dng, 258, 16);
    push_tiff_short(&mut dng, 259, 1);
    push_tiff_short(&mut dng, 262, 32_803);
    push_tiff_entry(&mut dng, 271, 2, 13, MAKE_OFFSET);
    push_tiff_entry(&mut dng, 272, 2, 13, MODEL_OFFSET);
    push_tiff_long(&mut dng, 273, RAW_OFFSET);
    push_tiff_short(&mut dng, 274, 1);
    push_tiff_short(&mut dng, 277, 1);
    push_tiff_long(&mut dng, 278, height);
    push_tiff_long(&mut dng, 279, raw_len);
    push_tiff_entry(&mut dng, 33_421, 3, 2, u32::from_le_bytes([2, 0, 2, 0]));
    push_tiff_entry(&mut dng, 33_422, 1, 4, u32::from_le_bytes([0, 1, 1, 2]));
    push_tiff_entry(&mut dng, 50_706, 1, 4, u32::from_le_bytes([1, 4, 0, 0]));
    push_tiff_entry(&mut dng, 50_708, 2, 13, UNIQUE_MODEL_OFFSET);
    push_tiff_short(&mut dng, 50_717, u16::MAX);
    push_tiff_entry(&mut dng, 50_721, 10, 9, COLOR_MATRIX_OFFSET);
    push_tiff_entry(&mut dng, 50_728, 5, 3, NEUTRAL_OFFSET);
    dng.extend_from_slice(&0_u32.to_le_bytes());

    dng.extend_from_slice(CAMERA);
    dng.push(0);
    dng.extend_from_slice(CAMERA);
    dng.push(0);
    dng.extend_from_slice(CAMERA);
    dng.push(0);
    for row in 0..3 {
        for column in 0..3 {
            let numerator = i32::from(row == column);
            dng.extend_from_slice(&numerator.to_le_bytes());
            dng.extend_from_slice(&1_i32.to_le_bytes());
        }
    }
    for _ in 0..3 {
        dng.extend_from_slice(&1_u32.to_le_bytes());
        dng.extend_from_slice(&1_u32.to_le_bytes());
    }
    assert_eq!(dng.len(), usize::try_from(RAW_OFFSET).unwrap());
    for y in 0..height {
        for x in 0..width {
            let base = match (y % 2, x % 2) {
                (0, 0) => 52_000_u16,
                (1, 1) => 12_000_u16,
                _ => 30_000_u16,
            };
            let detail = u16::try_from((x + y) * 16).unwrap();
            dng.extend_from_slice(&base.saturating_add(detail).to_le_bytes());
        }
    }
    std::fs::write(path, dng).unwrap();
}

fn push_tiff_short(output: &mut Vec<u8>, tag: u16, value: u16) {
    push_tiff_entry(output, tag, 3, 1, u32::from(value));
}

fn push_tiff_long(output: &mut Vec<u8>, tag: u16, value: u32) {
    push_tiff_entry(output, tag, 4, 1, value);
}

fn push_tiff_entry(output: &mut Vec<u8>, tag: u16, field_type: u16, count: u32, value: u32) {
    output.extend_from_slice(&tag.to_le_bytes());
    output.extend_from_slice(&field_type.to_le_bytes());
    output.extend_from_slice(&count.to_le_bytes());
    output.extend_from_slice(&value.to_le_bytes());
}
