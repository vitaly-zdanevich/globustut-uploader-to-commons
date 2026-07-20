use anyhow::{Context, Result, bail};
use std::env;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

pub const DEFAULT_MAX_FILE_MB: u64 = 200;
pub const DEFAULT_MAX_REQUEST_MB: u64 = 1_000;
pub const DEFAULT_MAX_FILES: usize = 20;
pub const DEFAULT_DISK_RESERVE_MB: u64 = 512;
pub const DEFAULT_MAX_PENDING_MB: u64 = 20_000;
pub const DEFAULT_MAX_PIXELS: u64 = 250_000_000;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    pub data_dir: PathBuf,
    pub upload_dir: PathBuf,
    pub thumbnail_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub database_path: PathBuf,
    pub max_file_bytes: u64,
    pub max_request_bytes: usize,
    pub max_files: usize,
    pub disk_reserve_bytes: u64,
    pub max_pending_bytes: u64,
    pub max_pixels: u64,
    pub conversion_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub session_ttl_seconds: i64,
    pub cookie_secure: bool,
    pub public_base_url: String,
    pub commons_api_url: String,
    pub wikidata_sparql_url: String,
    pub nearby_radius_meters: u32,
    pub nearby_limit: u32,
    pub user_agent: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let configured_data_dir = nonempty_env("TOOL_DATA_DIR");
        let tool_name = nonempty_env("TOOL_NAME")
            .or_else(|| nonempty_env("TOOLFORGE_TOOL"))
            .or_else(|| {
                configured_data_dir
                    .as_deref()
                    .and_then(tool_name_from_data_dir)
            });
        let data_dir = configured_data_dir
            .map(PathBuf::from)
            .map(|path| {
                if is_toolforge_data_root(&path) {
                    path.join("globustut-uploader")
                } else {
                    path
                }
            })
            .or_else(|| {
                tool_name.as_ref().map(|name| {
                    PathBuf::from("/data/project")
                        .join(name)
                        .join("globustut-uploader")
                })
            })
            .unwrap_or_else(|| PathBuf::from("data"));
        let upload_dir = nonempty_env("UPLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("pending"));
        let thumbnail_dir = nonempty_env("THUMBNAIL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("thumbnails"));
        let temp_dir = nonempty_env("APP_TEMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("globustut-uploader"));
        let database_path = nonempty_env("DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("globustut.sqlite3"));

        let port = parse_env("PORT", 8000_u16)?;
        let bind_ip = nonempty_env("BIND_ADDR")
            .map(|value| value.parse::<IpAddr>())
            .transpose()
            .context("BIND_ADDR must be an IP address")?
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let public_base_url = nonempty_env("PUBLIC_BASE_URL")
            .or_else(|| {
                tool_name
                    .as_ref()
                    .map(|name| format!("https://{name}.toolforge.org"))
            })
            .unwrap_or_else(|| format!("http://127.0.0.1:{port}"));
        let cookie_secure =
            parse_bool_env("COOKIE_SECURE", public_base_url.starts_with("https://"))?;

        let max_request_mb = parse_env("MAX_REQUEST_MB", DEFAULT_MAX_REQUEST_MB)?;
        let max_request_bytes_u64 = mb(max_request_mb)?;
        let max_request_bytes = usize::try_from(max_request_bytes_u64)
            .context("MAX_REQUEST_MB is too large for this platform")?;
        Ok(Self {
            bind: SocketAddr::new(bind_ip, port),
            data_dir,
            upload_dir,
            thumbnail_dir,
            temp_dir,
            database_path,
            max_file_bytes: mb(parse_env("MAX_FILE_MB", DEFAULT_MAX_FILE_MB)?)?,
            max_request_bytes,
            max_files: parse_env("MAX_FILES", DEFAULT_MAX_FILES)?,
            disk_reserve_bytes: mb(parse_env("DISK_RESERVE_MB", DEFAULT_DISK_RESERVE_MB)?)?,
            max_pending_bytes: mb(parse_env("MAX_PENDING_MB", DEFAULT_MAX_PENDING_MB)?)?,
            max_pixels: parse_env("MAX_PIXELS", DEFAULT_MAX_PIXELS)?,
            conversion_timeout_seconds: parse_env("CONVERSION_TIMEOUT_SECONDS", 120_u64)?,
            request_timeout_seconds: parse_env("REQUEST_TIMEOUT_SECONDS", 900_u64)?,
            session_ttl_seconds: parse_env("SESSION_TTL_HOURS", 12_i64)?
                .checked_mul(3600)
                .context("SESSION_TTL_HOURS is too large")?,
            cookie_secure,
            public_base_url,
            commons_api_url: nonempty_env("COMMONS_API_URL")
                .unwrap_or_else(|| "https://commons.wikimedia.org/w/api.php".into()),
            wikidata_sparql_url: nonempty_env("WIKIDATA_SPARQL_URL")
                .unwrap_or_else(|| "https://query.wikidata.org/sparql".into()),
            nearby_radius_meters: parse_env("NEARBY_RADIUS_METERS", 10_000_u32)?,
            nearby_limit: parse_env("NEARBY_CATEGORY_LIMIT", 20_u32)?.clamp(1, 20),
            user_agent: format!(
                "globustut-uploader-to-commons/{} (https://globustut.by/) ",
                env!("CARGO_PKG_VERSION")
            ),
        })
    }

    pub fn prepare_directories(&self) -> Result<()> {
        for directory in [
            &self.data_dir,
            &self.upload_dir,
            &self.thumbnail_dir,
            &self.temp_dir,
        ] {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("failed to create {}", directory.display()))?;
            set_private_directory_permissions(directory)?;
        }
        if let Some(parent) = self.database_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
            set_private_directory_permissions(parent)?;
        }
        Ok(())
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn tool_name_from_data_dir(raw: &str) -> Option<String> {
    let path = std::path::Path::new(raw);
    let relative = path.strip_prefix("/data/project").ok()?;
    let mut components = relative.components();
    let std::path::Component::Normal(name) = components.next()? else {
        return None;
    };
    let remainder = components.collect::<PathBuf>();
    if !remainder.as_os_str().is_empty() && remainder != std::path::Path::new("globustut-uploader")
    {
        return None;
    }
    name.to_str().map(str::to_string)
}

fn is_toolforge_data_root(path: &std::path::Path) -> bool {
    path.parent() == Some(std::path::Path::new("/data/project"))
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    nonempty_env(name)
        .map(|value| {
            value
                .parse::<T>()
                .with_context(|| format!("invalid {name}"))
        })
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn parse_bool_env(name: &str, default: bool) -> Result<bool> {
    let Some(value) = nonempty_env(name) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => bail!("{name} must be true/false or 1/0"),
    }
}

fn mb(value: u64) -> Result<u64> {
    value.checked_mul(1024 * 1024).context("size is too large")
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if path == std::path::Path::new("/")
        || path == std::path::Path::new("/tmp")
        || is_toolforge_data_root(path)
    {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{mb, tool_name_from_data_dir};

    #[test]
    fn converts_megabytes_without_overflow() {
        assert_eq!(mb(2).unwrap(), 2 * 1024 * 1024);
        assert!(mb(u64::MAX).is_err());
    }

    #[test]
    fn derives_tool_name_only_from_toolforge_data_root() {
        assert_eq!(
            tool_name_from_data_dir("/data/project/globustut"),
            Some("globustut".into())
        );
        assert_eq!(tool_name_from_data_dir("./data"), None);
        assert_eq!(
            tool_name_from_data_dir("/data/project/globustut/subdir"),
            None
        );
        assert_eq!(
            tool_name_from_data_dir("/data/project/globustut/globustut-uploader"),
            Some("globustut".into())
        );
    }
}
