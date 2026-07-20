use crate::auth::now_ts;
use crate::commons::validate_category;
use crate::config::Config;
use crate::db::{Database, UploadRecord};
use anyhow::{Context, Result, bail};
use exif::{In, Tag, Value};
use serde::Deserialize;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Semaphore;
use uuid::Uuid;

const WEBP_QUALITY: u8 = 92;
const MAX_COMMONS_FILENAME_BYTES: usize = 240;

#[derive(Clone)]
pub struct MediaProcessor {
    config: Arc<Config>,
    db: Database,
    conversion_slots: Arc<Semaphore>,
}

#[derive(Clone, Debug)]
pub struct DiskSpace {
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct ImageMetadata {
    latitude: Option<f64>,
    longitude: Option<f64>,
    capture_date: Option<String>,
}

struct RemoveFileOnDrop(Option<PathBuf>);

impl RemoveFileOnDrop {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            std::fs::remove_file(path).ok();
        }
    }
}

#[derive(Clone, Debug)]
enum MediaProbe {
    Image(ImageProbe),
    Video(VideoProbe),
}

#[derive(Clone, Debug)]
struct ImageProbe {
    extension: &'static str,
    mime_type: &'static str,
    width: u32,
    height: u32,
    pass_through: bool,
    lossless_conversion: bool,
}

#[derive(Clone, Debug)]
struct VideoProbe {
    extension: &'static str,
    mime_type: &'static str,
    width: Option<u32>,
    height: Option<u32>,
}

impl MediaProcessor {
    pub fn new(config: Arc<Config>, db: Database) -> Self {
        Self {
            config,
            db,
            conversion_slots: Arc::new(Semaphore::new(2)),
        }
    }

    pub async fn ingest(
        &self,
        temporary_path: &Path,
        original_filename: &str,
        commons_category: &str,
        license: &str,
    ) -> Result<UploadRecord> {
        let original_size_bytes = tokio::fs::metadata(temporary_path)
            .await
            .context("failed to inspect temporary upload")?
            .len();
        if original_size_bytes == 0 {
            bail!("the file is empty");
        }
        if original_size_bytes > self.config.max_file_bytes {
            bail!(
                "the file is larger than the {} MiB limit",
                self.config.max_file_bytes / (1024 * 1024)
            );
        }
        let (_, pending_bytes) = self
            .db
            .upload_totals()
            .await
            .context("failed to measure the pending queue")?;
        if pending_bytes.saturating_add(original_size_bytes) > self.config.max_pending_bytes {
            bail!("the pending moderation queue has reached its storage limit");
        }
        self.ensure_disk_space(original_size_bytes).await?;

        let probe = self.probe(temporary_path).await?;
        let category = validate_category(commons_category)?;
        let license = validate_license(license)?;
        let id = Uuid::new_v4().to_string();
        let mut converted_cleanup = None;

        let (source_path, extension, mime_type, media_kind, converted, width, height, metadata) =
            match probe {
                MediaProbe::Image(image) => {
                    validate_dimensions(image.width, image.height, self.config.max_pixels)?;
                    let metadata = extract_metadata(temporary_path).await;
                    if image.pass_through {
                        (
                            temporary_path.to_path_buf(),
                            image.extension,
                            image.mime_type,
                            "image",
                            false,
                            Some(image.width),
                            Some(image.height),
                            metadata,
                        )
                    } else {
                        let converted_path = self.config.temp_dir.join(format!("{id}.webp"));
                        converted_cleanup = Some(RemoveFileOnDrop::new(converted_path.clone()));
                        let _permit = self
                            .conversion_slots
                            .acquire()
                            .await
                            .context("conversion queue closed")?;
                        self.convert_image(
                            temporary_path,
                            &converted_path,
                            image.lossless_conversion,
                        )
                        .await?;
                        let converted_probe = self.probe_image(&converted_path).await?;
                        validate_dimensions(
                            converted_probe.width,
                            converted_probe.height,
                            self.config.max_pixels,
                        )?;
                        (
                            converted_path,
                            "webp",
                            "image/webp",
                            "image",
                            true,
                            Some(converted_probe.width),
                            Some(converted_probe.height),
                            metadata,
                        )
                    }
                }
                MediaProbe::Video(video) => (
                    temporary_path.to_path_buf(),
                    video.extension,
                    video.mime_type,
                    "video",
                    false,
                    video.width,
                    video.height,
                    ImageMetadata::default(),
                ),
            };

        let stored_filename = format!("{id}.{extension}");
        let stored_path = self.config.upload_dir.join(&stored_filename);
        self.persist_file(&source_path, &stored_path).await?;
        let mut stored_cleanup = RemoveFileOnDrop::new(stored_path.clone());
        let size_bytes = tokio::fs::metadata(&stored_path)
            .await
            .context("failed to inspect persisted upload")?
            .len();
        self.ensure_disk_space(0).await?;

        let thumbnail_path = self.thumbnail_path(&id);
        let mut thumbnail_cleanup =
            (media_kind == "image").then(|| RemoveFileOnDrop::new(thumbnail_path.clone()));
        if media_kind == "image" {
            self.make_thumbnail(&stored_path, &thumbnail_path)
                .await
                .context("failed to create moderation thumbnail")?;
        }

        let record = UploadRecord {
            id,
            stored_filename,
            original_filename: safe_original_filename(original_filename),
            display_filename: initial_display_filename(original_filename, extension),
            mime_type: mime_type.to_string(),
            media_kind: media_kind.to_string(),
            size_bytes,
            original_size_bytes,
            converted,
            width,
            height,
            latitude: metadata.latitude,
            longitude: metadata.longitude,
            capture_date: metadata.capture_date,
            commons_category: category,
            license,
            uploader_username: "anonymous".into(),
            created_at: now_ts(),
        };
        if let Err(error) = self.db.insert_upload(record.clone()).await {
            return Err(error.context("failed to record upload"));
        }
        stored_cleanup.disarm();
        if let Some(cleanup) = thumbnail_cleanup.as_mut() {
            cleanup.disarm();
        }
        drop(converted_cleanup);
        Ok(record)
    }

    pub fn stored_path(&self, upload: &UploadRecord) -> Result<PathBuf> {
        safe_child_path(&self.config.upload_dir, &upload.stored_filename)
    }

    pub fn thumbnail_path(&self, id: &str) -> PathBuf {
        self.config.thumbnail_dir.join(format!("{id}.webp"))
    }

    pub async fn delete_files(&self, upload: &UploadRecord) -> Result<()> {
        let stored_path = self.stored_path(upload)?;
        let mut failures = Vec::new();
        match tokio::fs::remove_file(&stored_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{}: {error}", stored_path.display())),
        }
        let thumbnail_path = self.thumbnail_path(&upload.id);
        match tokio::fs::remove_file(&thumbnail_path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failures.push(format!("{}: {error}", thumbnail_path.display())),
        }
        if !failures.is_empty() {
            bail!("failed to delete media files: {}", failures.join("; "));
        }
        Ok(())
    }

    pub async fn cleanup_orphaned_files(&self) -> Result<usize> {
        let references = self.db.storage_references().await?;
        let expected_uploads = references
            .iter()
            .map(|(_, stored_filename, _)| stored_filename.clone())
            .collect::<HashSet<_>>();
        let expected_thumbnails = references
            .iter()
            .filter(|(_, _, media_kind)| media_kind == "image")
            .map(|(id, _, _)| format!("{id}.webp"))
            .collect::<HashSet<_>>();

        let mut removed =
            remove_unreferenced_files(&self.config.upload_dir, &expected_uploads).await?;
        removed +=
            remove_unreferenced_files(&self.config.thumbnail_dir, &expected_thumbnails).await?;
        removed += remove_stale_temporary_files(&self.config.temp_dir).await?;
        Ok(removed)
    }

    pub async fn disk_space(&self) -> Result<DiskSpace> {
        let path = self.config.upload_dir.clone();
        tokio::task::spawn_blocking(move || {
            let stats = nix::sys::statvfs::statvfs(&path)
                .with_context(|| format!("failed to read free space for {}", path.display()))?;
            Ok(DiskSpace {
                available_bytes: stats
                    .blocks_available()
                    .saturating_mul(stats.fragment_size()),
                total_bytes: stats.blocks().saturating_mul(stats.fragment_size()),
            })
        })
        .await
        .context("disk-space worker stopped unexpectedly")?
    }

    async fn ensure_disk_space(&self, incoming_bytes: u64) -> Result<()> {
        let available = self.disk_space().await?.available_bytes;
        let required = self
            .config
            .disk_reserve_bytes
            .saturating_add(incoming_bytes);
        if available < required {
            bail!("not enough server storage is available for this file");
        }
        Ok(())
    }

    async fn probe(&self, path: &Path) -> Result<MediaProbe> {
        match self.probe_image(path).await {
            Ok(image) => Ok(MediaProbe::Image(image)),
            Err(image_error) => {
                match probe_video(path, self.config.conversion_timeout_seconds).await {
                    Ok(video) => Ok(MediaProbe::Video(video)),
                    Err(video_error) => bail!(
                        "unsupported or invalid media file (image probe: {image_error}; video probe: {video_error})"
                    ),
                }
            }
        }
    }

    async fn probe_image(&self, path: &Path) -> Result<ImageProbe> {
        let timeout = self.config.conversion_timeout_seconds;
        if let Ok(loader) = command_text(
            "vipsheader",
            [
                OsStr::new("-f"),
                OsStr::new("vips-loader"),
                path.as_os_str(),
            ],
            timeout,
        )
        .await
        {
            let width = vips_u32_field(path, "width", timeout).await?;
            let height = vips_u32_field(path, "height", timeout).await?;
            return Ok(image_probe_from_vips_loader(&loader, width, height));
        }

        let identity = imagemagick_identity(path, timeout).await?;
        Ok(image_probe_from_magick(
            &identity.format,
            identity.width,
            identity.height,
        ))
    }

    async fn convert_image(&self, input: &Path, output: &Path, lossless: bool) -> Result<()> {
        let timeout = self.config.conversion_timeout_seconds;
        let output_options = if lossless {
            format!("{}[lossless]", output.display())
        } else {
            format!("{}[Q={WEBP_QUALITY}]", output.display())
        };
        let vips = command_text(
            "vips",
            [
                OsStr::new("autorot"),
                input.as_os_str(),
                OsStr::new(&output_options),
            ],
            timeout,
        )
        .await;
        if vips.is_ok() && nonempty_file(output).await {
            return Ok(());
        }

        tokio::fs::remove_file(output).await.ok();
        let mut imagemagick_errors = Vec::new();
        for program in ["magick", "convert"] {
            let mut command = Command::new(program);
            command
                .arg("-limit")
                .arg("memory")
                .arg("1GiB")
                .arg("-limit")
                .arg("map")
                .arg("2GiB")
                .arg(input)
                .arg("-auto-orient");
            if lossless {
                command.arg("-define").arg("webp:lossless=true");
            } else {
                command.arg("-quality").arg(WEBP_QUALITY.to_string());
            }
            command.arg(output);
            let result = run_command(command, timeout).await;
            if result.is_ok() && nonempty_file(output).await {
                return Ok(());
            }
            imagemagick_errors.push(format!("{program}: {}", display_error(result)));
            tokio::fs::remove_file(output).await.ok();
        }
        bail!(
            "image conversion failed with libvips ({}) and ImageMagick ({})",
            display_error(vips),
            imagemagick_errors.join("; ")
        )
    }

    async fn make_thumbnail(&self, input: &Path, output: &Path) -> Result<()> {
        let output_options = format!("{}[Q=82]", output.display());
        let result = command_text(
            "vips",
            [
                OsStr::new("thumbnail"),
                input.as_os_str(),
                OsStr::new(&output_options),
                OsStr::new("720"),
                OsStr::new("--height"),
                OsStr::new("540"),
                OsStr::new("--size"),
                OsStr::new("down"),
            ],
            self.config.conversion_timeout_seconds,
        )
        .await;
        if result.is_err() || !nonempty_file(output).await {
            bail!(
                "libvips could not render a thumbnail: {}",
                display_error(result)
            );
        }
        set_private_file_permissions(output).await?;
        Ok(())
    }

    async fn persist_file(&self, source: &Path, destination: &Path) -> Result<()> {
        let part = destination.with_extension("part");
        let _part_cleanup = RemoveFileOnDrop::new(part.clone());
        let mut source_file = tokio::fs::File::open(source)
            .await
            .context("failed to reopen temporary upload")?;
        let mut destination_file = create_private_file(&part).await?;
        tokio::io::copy(&mut source_file, &mut destination_file)
            .await
            .context("failed to persist upload")?;
        destination_file
            .flush()
            .await
            .context("failed to flush upload")?;
        destination_file
            .sync_all()
            .await
            .context("failed to sync upload")?;
        drop(destination_file);
        tokio::fs::rename(&part, destination)
            .await
            .context("failed to finalize upload")?;
        Ok(())
    }
}

pub async fn write_multipart_file(
    field: &mut axum::extract::multipart::Field<'_>,
    path: &Path,
    max_bytes: u64,
) -> Result<u64> {
    let mut file = create_private_file(path).await?;
    let mut written = 0_u64;
    while let Some(chunk) = field
        .chunk()
        .await
        .context("failed while reading multipart upload")?
    {
        written = written
            .checked_add(chunk.len() as u64)
            .context("upload size overflow")?;
        if written > max_bytes {
            drop(file);
            tokio::fs::remove_file(path).await.ok();
            bail!("file exceeds the configured size limit");
        }
        file.write_all(&chunk)
            .await
            .context("failed to buffer upload")?;
    }
    file.flush().await.context("failed to flush upload")?;
    Ok(written)
}

pub fn renamed_display_filename(raw_basename: &str, existing_filename: &str) -> Result<String> {
    let extension = existing_filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty())
        .context("stored display filename has no extension")?;
    let mut basename = raw_basename.trim();
    if let Some(without_extension) = basename
        .strip_suffix(&format!(".{extension}"))
        .or_else(|| basename.strip_suffix(&format!(".{}", extension.to_uppercase())))
    {
        basename = without_extension;
    }
    let basename = sanitize_basename(basename);
    let max_basename_bytes = MAX_COMMONS_FILENAME_BYTES.saturating_sub(extension.len() + 1);
    let basename = truncate_utf8_bytes(&basename, max_basename_bytes);
    if basename.is_empty() {
        bail!("filename must not be empty");
    }
    Ok(format!("{basename}.{extension}"))
}

pub fn display_basename(filename: &str) -> &str {
    filename
        .rsplit_once('.')
        .map(|(basename, _)| basename)
        .unwrap_or(filename)
}

fn validate_license(raw: &str) -> Result<String> {
    match raw {
        "cc-by-sa-4.0" | "cc-by-4.0" | "cc0" => Ok(raw.to_string()),
        _ => bail!("invalid free license"),
    }
}

fn validate_dimensions(width: u32, height: u32, max_pixels: u64) -> Result<()> {
    if width == 0 || height == 0 {
        bail!("image dimensions are invalid");
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > max_pixels {
        bail!("image dimensions exceed the {max_pixels}-pixel safety limit");
    }
    Ok(())
}

fn image_probe_from_vips_loader(loader: &str, width: u32, height: u32) -> ImageProbe {
    let loader = loader.trim().to_ascii_lowercase();
    let known = if loader.contains("jpegload") {
        Some(("jpg", "image/jpeg"))
    } else if loader.contains("pngload") {
        Some(("png", "image/png"))
    } else if loader.contains("gifload") {
        Some(("gif", "image/gif"))
    } else if loader.contains("tiffload") {
        Some(("tif", "image/tiff"))
    } else if loader.contains("webpload") {
        Some(("webp", "image/webp"))
    } else {
        None
    };
    match known {
        Some((extension, mime_type)) => ImageProbe {
            extension,
            mime_type,
            width,
            height,
            pass_through: true,
            lossless_conversion: false,
        },
        None => ImageProbe {
            extension: "webp",
            mime_type: "image/webp",
            width,
            height,
            pass_through: false,
            lossless_conversion: loader.contains("svg")
                || loader.contains("bmp")
                || loader.contains("ppm"),
        },
    }
}

fn image_probe_from_magick(format: &str, width: u32, height: u32) -> ImageProbe {
    let format = format.trim().to_ascii_uppercase();
    let known = match format.as_str() {
        "JPEG" | "JPG" => Some(("jpg", "image/jpeg")),
        "PNG" => Some(("png", "image/png")),
        "GIF" => Some(("gif", "image/gif")),
        "TIFF" | "TIF" => Some(("tif", "image/tiff")),
        "WEBP" => Some(("webp", "image/webp")),
        _ => None,
    };
    match known {
        Some((extension, mime_type)) => ImageProbe {
            extension,
            mime_type,
            width,
            height,
            pass_through: true,
            lossless_conversion: false,
        },
        None => ImageProbe {
            extension: "webp",
            mime_type: "image/webp",
            width,
            height,
            pass_through: false,
            lossless_conversion: matches!(format.as_str(), "BMP" | "SVG" | "PBM" | "PGM" | "PPM"),
        },
    }
}

#[derive(Debug)]
struct ImageMagickIdentity {
    format: String,
    width: u32,
    height: u32,
}

async fn imagemagick_identity(path: &Path, timeout: u64) -> Result<ImageMagickIdentity> {
    let first_frame = format!("{}[0]", path.display());
    let modern = command_text(
        "magick",
        [
            OsStr::new("identify"),
            OsStr::new("-quiet"),
            OsStr::new("-format"),
            OsStr::new("%m %w %h"),
            OsStr::new(&first_frame),
        ],
        timeout,
    )
    .await;
    let text = match modern {
        Ok(text) => text,
        Err(modern_error) => command_text(
            "identify",
            [
                OsStr::new("-quiet"),
                OsStr::new("-format"),
                OsStr::new("%m %w %h"),
                OsStr::new(&first_frame),
            ],
            timeout,
        )
        .await
        .with_context(|| format!("ImageMagick 7 probe failed too ({modern_error})"))?,
    };
    let mut fields = text.split_whitespace();
    let format = fields.next().context("ImageMagick omitted image format")?;
    let width = fields
        .next()
        .context("ImageMagick omitted image width")?
        .parse()
        .context("ImageMagick returned invalid image width")?;
    let height = fields
        .next()
        .context("ImageMagick omitted image height")?
        .parse()
        .context("ImageMagick returned invalid image height")?;
    Ok(ImageMagickIdentity {
        format: format.to_string(),
        width,
        height,
    })
}

async fn vips_u32_field(path: &Path, field: &str, timeout: u64) -> Result<u32> {
    command_text(
        "vipsheader",
        [OsStr::new("-f"), OsStr::new(field), path.as_os_str()],
        timeout,
    )
    .await?
    .trim()
    .parse()
    .with_context(|| format!("libvips returned invalid {field}"))
}

#[derive(Debug, Deserialize)]
struct FfprobeResponse {
    format: Option<FfprobeFormat>,
    #[serde(default)]
    streams: Vec<FfprobeStream>,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

async fn probe_video(path: &Path, timeout: u64) -> Result<VideoProbe> {
    let text = command_text(
        "ffprobe",
        [
            OsStr::new("-v"),
            OsStr::new("error"),
            OsStr::new("-show_entries"),
            OsStr::new("format=format_name:stream=codec_type,codec_name,width,height"),
            OsStr::new("-of"),
            OsStr::new("json"),
            path.as_os_str(),
        ],
        timeout,
    )
    .await?;
    let probe: FfprobeResponse =
        serde_json::from_str(&text).context("ffprobe returned invalid JSON")?;
    let format = probe
        .format
        .and_then(|format| format.format_name)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let video_streams = probe
        .streams
        .iter()
        .filter(|stream| stream.codec_type.as_deref() == Some("video"))
        .collect::<Vec<_>>();
    if video_streams.is_empty() {
        bail!("file has no video stream");
    }
    let supported_audio = |codec: &str| matches!(codec, "opus" | "vorbis" | "mp1" | "mp2");
    let all_audio_supported = probe.streams.iter().all(|stream| {
        stream.codec_type.as_deref() != Some("audio")
            || stream.codec_name.as_deref().is_some_and(supported_audio)
    });
    let (extension, mime_type, supported_video) = if format.contains("webm") {
        (
            "webm",
            "video/webm",
            video_streams.iter().all(|stream| {
                stream
                    .codec_name
                    .as_deref()
                    .is_some_and(|codec| matches!(codec, "vp8" | "vp9" | "av1"))
            }),
        )
    } else if format.contains("ogg") {
        (
            "ogv",
            "video/ogg",
            video_streams
                .iter()
                .all(|stream| stream.codec_name.as_deref() == Some("theora")),
        )
    } else if format.contains("mpeg") {
        (
            "mpeg",
            "video/mpeg",
            video_streams.iter().all(|stream| {
                stream
                    .codec_name
                    .as_deref()
                    .is_some_and(|codec| matches!(codec, "mpeg1video" | "mpeg2video"))
            }),
        )
    } else {
        bail!("video container is not supported by Wikimedia Commons");
    };
    if !supported_video || !all_audio_supported {
        bail!(
            "video codecs are not supported by Wikimedia Commons; video conversion is not in this MVP"
        );
    }
    Ok(VideoProbe {
        extension,
        mime_type,
        width: video_streams.first().and_then(|stream| stream.width),
        height: video_streams.first().and_then(|stream| stream.height),
    })
}

async fn extract_metadata(path: &Path) -> ImageMetadata {
    let path_for_internal = path.to_path_buf();
    let internal = tokio::task::spawn_blocking(move || extract_exif_internal(&path_for_internal))
        .await
        .unwrap_or_default();
    let external = extract_with_exiftool(path).await.unwrap_or_default();
    ImageMetadata {
        latitude: external
            .latitude
            .or(internal.latitude)
            .filter(|value| value.is_finite() && (-90.0..=90.0).contains(value)),
        longitude: external
            .longitude
            .or(internal.longitude)
            .filter(|value| value.is_finite() && (-180.0..=180.0).contains(value)),
        capture_date: external.capture_date.or(internal.capture_date),
    }
}

fn extract_exif_internal(path: &Path) -> ImageMetadata {
    let Ok(file) = std::fs::File::open(path) else {
        return ImageMetadata::default();
    };
    let mut reader = std::io::BufReader::new(file);
    let Ok(exif) = exif::Reader::new().read_from_container(&mut reader) else {
        return ImageMetadata::default();
    };
    ImageMetadata {
        latitude: gps_coordinate(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, 'S'),
        longitude: gps_coordinate(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, 'W'),
        capture_date: exif
            .get_field(Tag::DateTimeOriginal, In::PRIMARY)
            .or_else(|| exif.get_field(Tag::DateTime, In::PRIMARY))
            .and_then(exif_ascii)
            .and_then(|date| exif_date(&date)),
    }
}

fn gps_coordinate(
    exif: &exif::Exif,
    coordinate_tag: Tag,
    reference_tag: Tag,
    negative_reference: char,
) -> Option<f64> {
    let Value::Rational(parts) = &exif.get_field(coordinate_tag, In::PRIMARY)?.value else {
        return None;
    };
    if parts.len() < 3 {
        return None;
    }
    let degrees = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
    let negative = exif
        .get_field(reference_tag, In::PRIMARY)
        .and_then(exif_ascii)
        .is_some_and(|reference| reference.starts_with(negative_reference));
    Some(if negative { -degrees } else { degrees })
}

fn exif_ascii(field: &exif::Field) -> Option<String> {
    match &field.value {
        Value::Ascii(values) => values
            .first()
            .map(|value| String::from_utf8_lossy(value).trim().to_string()),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct ExifToolRecord {
    #[serde(rename = "GPSLatitude")]
    latitude: Option<f64>,
    #[serde(rename = "GPSLongitude")]
    longitude: Option<f64>,
    #[serde(rename = "DateTimeOriginal")]
    capture_date: Option<String>,
}

async fn extract_with_exiftool(path: &Path) -> Result<ImageMetadata> {
    let text = command_text(
        "exiftool",
        [
            OsStr::new("-json"),
            OsStr::new("-n"),
            OsStr::new("-GPSLatitude"),
            OsStr::new("-GPSLongitude"),
            OsStr::new("-DateTimeOriginal"),
            path.as_os_str(),
        ],
        20,
    )
    .await?;
    let mut records: Vec<ExifToolRecord> =
        serde_json::from_str(&text).context("ExifTool returned invalid JSON")?;
    let record = records
        .pop()
        .context("ExifTool returned no metadata record")?;
    Ok(ImageMetadata {
        latitude: record
            .latitude
            .filter(|value| (-90.0..=90.0).contains(value)),
        longitude: record
            .longitude
            .filter(|value| (-180.0..=180.0).contains(value)),
        capture_date: record.capture_date.and_then(|date| exif_date(&date)),
    })
}

fn exif_date(raw: &str) -> Option<String> {
    let value = raw.split_whitespace().next()?;
    let mut parts = value.split(':');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    let numeric = year.len() == 4
        && month.len() == 2
        && day.len() == 2
        && [year, month, day]
            .iter()
            .all(|part| part.bytes().all(|byte| byte.is_ascii_digit()));
    if !numeric {
        return None;
    }
    let year_number = year.parse().ok()?;
    let month_number = month.parse::<u8>().ok()?;
    let day_number = day.parse().ok()?;
    let month_value = time::Month::try_from(month_number).ok()?;
    time::Date::from_calendar_date(year_number, month_value, day_number).ok()?;
    Some(format!("{year}-{month}-{day}"))
}

fn safe_original_filename(raw: &str) -> String {
    let filename = raw.replace('\\', "/");
    let basename = filename.rsplit('/').next().unwrap_or("");
    let cleaned = basename
        .chars()
        .filter(|character| !character.is_control())
        .take(255)
        .collect::<String>();
    if cleaned.trim().is_empty() {
        "unnamed-upload".into()
    } else {
        cleaned
    }
}

fn initial_display_filename(original: &str, extension: &str) -> String {
    let safe = safe_original_filename(original);
    let basename = Path::new(&safe)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(sanitize_basename)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Photo".into());
    let max_basename_bytes = MAX_COMMONS_FILENAME_BYTES.saturating_sub(extension.len() + 1);
    format!(
        "{}.{}",
        truncate_utf8_bytes(&basename, max_basename_bytes),
        extension
    )
}

fn sanitize_basename(raw: &str) -> String {
    let mut result = raw
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>'
                        | ':'
                        | '"'
                        | '/'
                        | '\\'
                        | '|'
                        | '?'
                        | '*'
                        | '#'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '\u{fffd}'
                )
            {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    result = result.trim_matches([' ', '.']).chars().take(180).collect();
    while result.contains("~~~") {
        result = result.replace("~~~", "~~");
    }
    result
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value[..end].trim_end().to_string()
}

fn safe_child_path(parent: &Path, filename: &str) -> Result<PathBuf> {
    let path = Path::new(filename);
    let mut components = path.components();
    let safe =
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
    if !safe {
        bail!("invalid stored filename");
    }
    Ok(parent.join(path))
}

async fn create_private_file(path: &Path) -> Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    options
        .open(path)
        .await
        .with_context(|| format!("failed to create {}", path.display()))
}

async fn set_private_file_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .await
            .with_context(|| format!("failed to protect {}", path.display()))?;
    }
    Ok(())
}

async fn command_text<I, S>(program: &str, args: I, timeout_seconds: u64) -> Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(program);
    command.args(args);
    let output = run_command(command, timeout_seconds).await?;
    String::from_utf8(output.stdout).context("media tool returned non-UTF-8 output")
}

async fn run_command(mut command: Command, timeout_seconds: u64) -> Result<std::process::Output> {
    command.kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(timeout_seconds), command.output())
        .await
        .context("media tool timed out")?
        .context("failed to start media tool")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "media tool exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(output)
}

async fn nonempty_file(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.len() > 0)
}

async fn remove_unreferenced_files(directory: &Path, expected: &HashSet<String>) -> Result<usize> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .with_context(|| format!("failed to scan {}", directory.display()))?;
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !expected.contains(&name) {
            tokio::fs::remove_file(entry.path())
                .await
                .with_context(|| format!("failed to remove orphaned {}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

async fn remove_stale_temporary_files(directory: &Path) -> Result<usize> {
    let mut entries = tokio::fs::read_dir(directory)
        .await
        .with_context(|| format!("failed to scan {}", directory.display()))?;
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await? {
        if !entry.file_type().await?.is_file() {
            continue;
        }
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let upload_buffer = name
            .strip_prefix("upload-")
            .is_some_and(|id| Uuid::parse_str(id).is_ok());
        let converted = name
            .strip_suffix(".webp")
            .is_some_and(|id| Uuid::parse_str(id).is_ok());
        if upload_buffer || converted {
            tokio::fs::remove_file(entry.path())
                .await
                .with_context(|| format!("failed to remove stale {}", entry.path().display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn display_error<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => "no output".into(),
        Err(error) => format!("{error:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_COMMONS_FILENAME_BYTES, exif_date, image_probe_from_vips_loader,
        initial_display_filename, renamed_display_filename, safe_child_path, validate_category,
        validate_dimensions,
    };
    use std::path::Path;

    #[test]
    fn canonicalizes_display_names() {
        assert_eq!(
            initial_display_filename("C:\\fakepath\\My photo.HEIC", "webp"),
            "My photo.webp"
        );
        assert_eq!(
            renamed_display_filename("Нясвіжскі замак", "IMG_1.jpg").unwrap(),
            "Нясвіжскі замак.jpg"
        );
        assert_eq!(
            renamed_display_filename("Castle.jpg", "IMG_1.jpg").unwrap(),
            "Castle.jpg"
        );
        assert!(renamed_display_filename("...", "IMG_1.jpg").is_err());
        let long_name = renamed_display_filename(&"Ж".repeat(180), "IMG_1.jpg").unwrap();
        assert!(long_name.len() <= MAX_COMMONS_FILENAME_BYTES);
        assert_eq!(
            renamed_display_filename("Bad#[title]~~~", "IMG_1.jpg").unwrap(),
            "Bad title ~~.jpg"
        );
    }

    #[test]
    fn blocks_storage_path_traversal() {
        assert!(safe_child_path(Path::new("/tmp/uploads"), "one.jpg").is_ok());
        assert!(safe_child_path(Path::new("/tmp/uploads"), "../one.jpg").is_err());
        assert!(safe_child_path(Path::new("/tmp/uploads"), "/etc/passwd").is_err());
    }

    #[test]
    fn identifies_commons_formats_and_conversion_inputs() {
        let jpeg = image_probe_from_vips_loader("jpegload", 10, 20);
        assert!(jpeg.pass_through);
        assert_eq!(jpeg.extension, "jpg");
        let heic = image_probe_from_vips_loader("heifload", 10, 20);
        assert!(!heic.pass_through);
        assert_eq!(heic.extension, "webp");
    }

    #[test]
    fn validates_categories_dimensions_and_dates() {
        assert_eq!(
            validate_category("Category:Castles_in_Belarus").unwrap(),
            "Castles in Belarus"
        );
        assert!(validate_category("\n").is_err());
        assert!(validate_dimensions(10, 20, 200).is_ok());
        assert!(validate_dimensions(10, 21, 200).is_err());
        assert_eq!(
            exif_date("2025:07:18 12:13:14").as_deref(),
            Some("2025-07-18")
        );
        assert!(exif_date("2025:99:99 12:13:14").is_none());
    }
}
