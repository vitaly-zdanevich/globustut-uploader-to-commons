use crate::config::Config;
use crate::db::UploadRecord;
use anyhow::{Context, Result, bail};
use reqwest::StatusCode;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;

pub const COMMONS_ACCOUNT: &str = "Globustut";
pub const COMMONS_BOT_USERNAME: &str = "Globustut@globustut-uploader";
const CHUNK_SIZE: usize = 16 * 1024 * 1024;
const UPLOAD_COMMENT: &str = "Запампавана пасля прэмадэрацыі на globustut.by";
const AMBIGUOUS_RECONCILIATION_ATTEMPTS: usize = 3;
const AMBIGUOUS_RECONCILIATION_DELAY: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct CommonsPublisher {
    inner: Arc<PublisherInner>,
}

struct PublisherInner {
    client: reqwest::Client,
    api_url: String,
    password: String,
    session: Mutex<Option<AuthContext>>,
    operation: Mutex<()>,
}

#[derive(Clone)]
struct AuthContext {
    username: String,
    csrf_token: String,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PublishedFile {
    pub filename: String,
    pub page_url: String,
    pub file_url: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct UploadPayload {
    result: Option<String>,
    offset: Option<u64>,
    filekey: Option<String>,
    filename: Option<String>,
    warnings: Option<Map<String, Value>>,
    imageinfo: Option<ImageInfo>,
}

#[derive(Debug, Default, Deserialize)]
struct ImageInfo {
    url: Option<String>,
    descriptionurl: Option<String>,
}

#[derive(Clone, Debug)]
struct ExistingFile {
    filename: String,
    page_url: Option<String>,
    file_url: Option<String>,
    uploader: Option<String>,
    comment: Option<String>,
    sha1: Option<String>,
}

impl ExistingFile {
    fn matches_upload(&self, sha1: &str) -> bool {
        self.sha1
            .as_deref()
            .is_some_and(|existing| imageinfo_sha1_matches(existing, sha1))
            && self.uploader.as_deref() == Some(COMMONS_ACCOUNT)
            && self.comment.as_deref() == Some(UPLOAD_COMMENT)
    }

    fn into_published(self) -> PublishedFile {
        let Self {
            filename,
            page_url,
            file_url,
            ..
        } = self;
        PublishedFile {
            page_url: page_url.unwrap_or_else(|| file_page_url(&filename)),
            file_url: Some(file_url.unwrap_or_else(|| file_redirect_url(&filename))),
            filename,
        }
    }
}

#[derive(Debug)]
enum PublishError {
    Api { code: String, info: String },
    Warning(String),
    Transport(String),
    Protocol(String),
}

impl PublishError {
    fn is_authentication(&self) -> bool {
        matches!(
            self,
            Self::Api { code, .. }
                if matches!(
                    code.as_str(),
                    "assertuserfailed" | "badtoken" | "notloggedin" | "mustbeloggedin"
            )
        )
    }

    fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Transport(_) | Self::Protocol(_))
    }
}

impl fmt::Display for PublishError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { code, info } => {
                write!(formatter, "Wikimedia Commons API error {code}: {info}")
            }
            Self::Warning(message) => write!(formatter, "Wikimedia Commons warning: {message}"),
            Self::Transport(message) => {
                write!(formatter, "could not contact Wikimedia Commons: {message}")
            }
            Self::Protocol(message) => {
                write!(formatter, "invalid Wikimedia Commons response: {message}")
            }
        }
    }
}

impl std::error::Error for PublishError {}

impl CommonsPublisher {
    pub fn from_config(config: &Config) -> Result<Option<Self>> {
        let password = match std::fs::read_to_string(&config.commons_bot_password_path) {
            Ok(password) => password.trim_end_matches(['\r', '\n']).to_string(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to read {}",
                        config.commons_bot_password_path.display()
                    )
                });
            }
        };
        validate_bot_password(&password)?;
        validate_commons_api_url(&config.commons_api_url)?;
        let client = reqwest::Client::builder()
            .cookie_store(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(config.commons_api_timeout_seconds))
            .user_agent(&config.user_agent)
            .build()
            .context("failed to create Wikimedia Commons upload client")?;
        Ok(Some(Self {
            inner: Arc::new(PublisherInner {
                client,
                api_url: config.commons_api_url.clone(),
                password,
                session: Mutex::new(None),
                operation: Mutex::new(()),
            }),
        }))
    }

    pub async fn publish(&self, upload: &UploadRecord, path: &Path) -> Result<PublishedFile> {
        let _operation = self.inner.operation.lock().await;
        let first = self.publish_with_current_session(upload, path).await;
        match first {
            Err(error) if error.is_authentication() => {
                *self.inner.session.lock().await = None;
                self.publish_with_current_session(upload, path)
                    .await
                    .map_err(Into::into)
            }
            result => result.map_err(Into::into),
        }
    }

    async fn publish_with_current_session(
        &self,
        upload: &UploadRecord,
        path: &Path,
    ) -> std::result::Result<PublishedFile, PublishError> {
        let auth = self.auth_context().await?;
        if let Some(existing) = self.existing_file(&upload.display_filename).await? {
            let sha1 = mediawiki_sha1(path).await?;
            if existing.matches_upload(&sha1) {
                return Ok(existing.into_published());
            }
            return Err(PublishError::Warning(format!(
                "a file named {} already exists and does not match this upload",
                upload.display_filename
            )));
        }
        self.upload_file(
            &auth,
            path,
            &upload.display_filename,
            &build_wikitext(upload),
        )
        .await
    }

    async fn auth_context(&self) -> std::result::Result<AuthContext, PublishError> {
        if let Some(auth) = self.inner.session.lock().await.clone() {
            return Ok(auth);
        }
        let auth = self.login().await?;
        *self.inner.session.lock().await = Some(auth.clone());
        Ok(auth)
    }

    async fn login(&self) -> std::result::Result<AuthContext, PublishError> {
        let value = self
            .get(&[
                ("action", "query"),
                ("meta", "tokens"),
                ("type", "login"),
                ("format", "json"),
                ("formatversion", "2"),
            ])
            .await?;
        let login_token = value
            .pointer("/query/tokens/logintoken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| PublishError::Protocol("login token is missing".into()))?;
        let response = self
            .inner
            .client
            .post(&self.inner.api_url)
            .form(&[
                ("action", "login"),
                ("lgname", COMMONS_BOT_USERNAME),
                ("lgpassword", &self.inner.password),
                ("lgtoken", login_token),
                ("format", "json"),
                ("formatversion", "2"),
            ])
            .send()
            .await
            .map_err(transport)?;
        let value = parse_response(response).await?;
        let result = value
            .pointer("/login/result")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if result != "Success" {
            let reason = value
                .pointer("/login/reason")
                .and_then(Value::as_str)
                .unwrap_or(result);
            return Err(PublishError::Api {
                code: "login-failed".into(),
                info: reason.into(),
            });
        }

        let value = self
            .get(&[
                ("action", "query"),
                ("meta", "tokens|userinfo"),
                ("type", "csrf"),
                ("format", "json"),
                ("formatversion", "2"),
                ("maxlag", "5"),
            ])
            .await?;
        let username = value
            .pointer("/query/userinfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let csrf_token = value
            .pointer("/query/tokens/csrftoken")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if username != COMMONS_ACCOUNT || csrf_token.is_empty() || csrf_token == "+\\" {
            return Err(PublishError::Api {
                code: "login-failed".into(),
                info: "the BotPassword did not authenticate as the configured account".into(),
            });
        }
        Ok(AuthContext {
            username: username.into(),
            csrf_token: csrf_token.into(),
        })
    }

    async fn existing_file(
        &self,
        filename: &str,
    ) -> std::result::Result<Option<ExistingFile>, PublishError> {
        let title = format!("File:{filename}");
        let value = self
            .get(&[
                ("action", "query"),
                ("prop", "imageinfo"),
                ("iiprop", "url|user|comment|sha1"),
                ("iilimit", "1"),
                ("titles", title.as_str()),
                ("format", "json"),
                ("formatversion", "2"),
                ("maxlag", "5"),
            ])
            .await?;
        let pages = value
            .pointer("/query/pages")
            .and_then(Value::as_array)
            .ok_or_else(|| PublishError::Protocol("query.pages is missing".into()))?;
        let page = pages
            .first()
            .ok_or_else(|| PublishError::Protocol("query.pages is empty".into()))?;
        if page.get("missing").is_some() {
            return Ok(None);
        }
        let actual_filename = page
            .get("title")
            .and_then(Value::as_str)
            .and_then(|title| title.strip_prefix("File:"))
            .unwrap_or(filename)
            .to_owned();
        let info = page.pointer("/imageinfo/0");
        Ok(Some(ExistingFile {
            filename: actual_filename,
            page_url: optional_string(info, "descriptionurl"),
            file_url: optional_string(info, "url"),
            uploader: optional_string(info, "user"),
            comment: optional_string(info, "comment"),
            sha1: optional_string(info, "sha1"),
        }))
    }

    async fn upload_file(
        &self,
        auth: &AuthContext,
        path: &Path,
        filename: &str,
        wikitext: &str,
    ) -> std::result::Result<PublishedFile, PublishError> {
        let file_size = tokio::fs::metadata(path)
            .await
            .map_err(|error| PublishError::Protocol(error.to_string()))?
            .len();
        if file_size == 0 {
            return Err(PublishError::Protocol("stored file is empty".into()));
        }
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|error| PublishError::Protocol(error.to_string()))?;
        let mut offset = 0_u64;
        let mut filekey: Option<String> = None;

        while offset < file_size {
            let chunk_length = (file_size - offset).min(CHUNK_SIZE as u64) as usize;
            let mut chunk = vec![0_u8; chunk_length];
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|error| PublishError::Protocol(error.to_string()))?;
            file.read_exact(&mut chunk)
                .await
                .map_err(|error| PublishError::Protocol(error.to_string()))?;
            let mut form = base_upload_form(auth)
                .text("stash", "1")
                .text("ignorewarnings", "1")
                .text("filename", filename.to_owned())
                .text("filesize", file_size.to_string())
                .text("offset", offset.to_string())
                .part(
                    "chunk",
                    Part::bytes(chunk)
                        .file_name("chunk.bin")
                        .mime_str("application/octet-stream")
                        .map_err(|error| PublishError::Protocol(error.to_string()))?,
                );
            if let Some(key) = &filekey {
                form = form.text("filekey", key.clone());
            }
            let payload = self.post_upload(form).await?;
            if let Some(key) = payload.filekey {
                filekey = Some(key);
            }
            match payload.result.as_deref() {
                Some("Continue") => {
                    let next = payload.offset.ok_or_else(|| {
                        PublishError::Protocol("chunk response omitted offset".into())
                    })?;
                    if next <= offset || next > file_size {
                        return Err(PublishError::Protocol(format!(
                            "chunk response returned invalid offset {next}"
                        )));
                    }
                    offset = next;
                }
                Some("Success" | "Warning") => offset = file_size,
                other => {
                    return Err(PublishError::Protocol(format!(
                        "unexpected chunk result {}",
                        other.unwrap_or("none")
                    )));
                }
            }
        }

        let filekey = filekey
            .ok_or_else(|| PublishError::Protocol("chunk upload omitted file key".into()))?;
        let payload = match self
            .post_upload(
                base_upload_form(auth)
                    .text("filekey", filekey)
                    .text("filename", filename.to_owned())
                    .text("text", wikitext.to_owned())
                    .text("comment", UPLOAD_COMMENT),
            )
            .await
        {
            Ok(payload) => payload,
            Err(error) if error.is_ambiguous() => {
                return self.reconcile_ambiguous_upload(filename, path, error).await;
            }
            Err(error) => return Err(error),
        };
        if payload.result.as_deref() != Some("Success") {
            if let Some(warnings) = payload.warnings {
                return Err(PublishError::Warning(describe_warnings(&warnings)));
            }
            let error = PublishError::Protocol(format!(
                "unexpected publish result {}",
                payload.result.as_deref().unwrap_or("none")
            ));
            return self.reconcile_ambiguous_upload(filename, path, error).await;
        }
        let actual_filename = payload.filename.unwrap_or_else(|| filename.to_owned());
        let mut page_url = payload
            .imageinfo
            .as_ref()
            .and_then(|info| info.descriptionurl.clone());
        let mut file_url = payload.imageinfo.and_then(|info| info.url);
        if page_url.is_none() || file_url.is_none() {
            match self.image_urls(&actual_filename).await {
                Ok((queried_page, queried_file)) => {
                    page_url = page_url.or(queried_page);
                    file_url = file_url.or(queried_file);
                }
                Err(error) => {
                    tracing::warn!(
                        filename = %actual_filename,
                        error = %error,
                        "Commons accepted the upload but optional URL enrichment failed"
                    );
                }
            }
        }
        Ok(PublishedFile {
            filename: actual_filename.clone(),
            page_url: page_url.unwrap_or_else(|| file_page_url(&actual_filename)),
            file_url: Some(file_url.unwrap_or_else(|| file_redirect_url(&actual_filename))),
        })
    }

    async fn reconcile_ambiguous_upload(
        &self,
        filename: &str,
        path: &Path,
        original_error: PublishError,
    ) -> std::result::Result<PublishedFile, PublishError> {
        let sha1 = match mediawiki_sha1(path).await {
            Ok(sha1) => sha1,
            Err(error) => {
                tracing::warn!(
                    filename,
                    error = %error,
                    "could not hash a locally pending file after an ambiguous Commons response"
                );
                return Err(original_error);
            }
        };

        for attempt in 0..AMBIGUOUS_RECONCILIATION_ATTEMPTS {
            if attempt != 0 {
                tokio::time::sleep(AMBIGUOUS_RECONCILIATION_DELAY * attempt as u32).await;
            }
            match self.existing_file(filename).await {
                Ok(Some(existing)) if existing.matches_upload(&sha1) => {
                    tracing::warn!(
                        filename,
                        attempt = attempt + 1,
                        "reconciled an upload after an ambiguous Commons response"
                    );
                    return Ok(existing.into_published());
                }
                Ok(Some(_)) => return Err(original_error),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        filename,
                        attempt = attempt + 1,
                        error = %error,
                        "could not reconcile an ambiguous Commons response"
                    );
                }
            }
        }
        Err(original_error)
    }

    async fn image_urls(
        &self,
        filename: &str,
    ) -> std::result::Result<(Option<String>, Option<String>), PublishError> {
        let title = format!("File:{filename}");
        let value = self
            .get(&[
                ("action", "query"),
                ("prop", "imageinfo"),
                ("iiprop", "url"),
                ("titles", title.as_str()),
                ("format", "json"),
                ("formatversion", "2"),
                ("maxlag", "5"),
            ])
            .await?;
        let info = value
            .pointer("/query/pages/0/imageinfo/0")
            .and_then(Value::as_object);
        Ok((
            info.and_then(|item| item.get("descriptionurl"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            info.and_then(|item| item.get("url"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        ))
    }

    async fn get(&self, query: &[(&str, &str)]) -> std::result::Result<Value, PublishError> {
        let response = self
            .inner
            .client
            .get(&self.inner.api_url)
            .query(query)
            .send()
            .await
            .map_err(transport)?;
        parse_response(response).await
    }

    async fn post_upload(&self, form: Form) -> std::result::Result<UploadPayload, PublishError> {
        let response = self
            .inner
            .client
            .post(&self.inner.api_url)
            .multipart(form)
            .send()
            .await
            .map_err(transport)?;
        let value = parse_response(response).await?;
        let upload = value
            .get("upload")
            .cloned()
            .ok_or_else(|| PublishError::Protocol("upload object is missing".into()))?;
        serde_json::from_value(upload)
            .map_err(|error| PublishError::Protocol(format!("invalid upload object: {error}")))
    }
}

fn base_upload_form(auth: &AuthContext) -> Form {
    Form::new()
        .text("action", "upload")
        .text("format", "json")
        .text("formatversion", "2")
        .text("maxlag", "5")
        .text("assert", "user")
        .text("assertuser", auth.username.clone())
        .text("token", auth.csrf_token.clone())
}

async fn parse_response(response: reqwest::Response) -> std::result::Result<Value, PublishError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| PublishError::Transport(error.to_string()))?;
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(PublishError::Api {
            code: "ratelimited".into(),
            info: "upload rate limit reached".into(),
        });
    }
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        PublishError::Protocol(format!(
            "HTTP {status} returned invalid JSON ({error}): {}",
            compact(&text, 180)
        ))
    })?;
    if let Some(error) = value.get("error") {
        return Err(PublishError::Api {
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .into(),
            info: error
                .get("info")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .into(),
        });
    }
    if !status.is_success() {
        return Err(PublishError::Transport(format!("HTTP {status}")));
    }
    Ok(value)
}

fn transport(error: reqwest::Error) -> PublishError {
    PublishError::Transport(error.to_string())
}

fn describe_warnings(warnings: &Map<String, Value>) -> String {
    warnings
        .iter()
        .map(|(code, detail)| {
            let detail = detail
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| compact(&detail.to_string(), 120));
            format!("{code}: {detail}")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn compact(value: &str, maximum: usize) -> String {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if value.chars().count() <= maximum {
        return value;
    }
    value.chars().take(maximum).collect::<String>() + "…"
}

fn optional_string(value: Option<&Value>, key: &str) -> Option<String> {
    value?.get(key).and_then(Value::as_str).map(str::to_owned)
}

async fn mediawiki_sha1(path: &Path) -> std::result::Result<String, PublishError> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| PublishError::Protocol(error.to_string()))?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| PublishError::Protocol(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(mediawiki_sha1_base36(&hasher.finalize()))
}

fn mediawiki_sha1_base36(digest: &[u8]) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    const MEDIAWIKI_SHA1_LENGTH: usize = 31;

    let mut number = digest.to_vec();
    let mut encoded = Vec::with_capacity(MEDIAWIKI_SHA1_LENGTH);
    while number.iter().any(|byte| *byte != 0) {
        let mut remainder = 0_u16;
        for byte in &mut number {
            let value = remainder * 256 + u16::from(*byte);
            *byte = (value / 36) as u8;
            remainder = value % 36;
        }
        encoded.push(DIGITS[remainder as usize]);
    }
    if encoded.is_empty() {
        encoded.push(b'0');
    }
    while encoded.len() < MEDIAWIKI_SHA1_LENGTH {
        encoded.push(b'0');
    }
    encoded.reverse();
    String::from_utf8(encoded).expect("base-36 digits are valid UTF-8")
}

fn imageinfo_sha1_matches(imageinfo_sha1: &str, local_base36: &str) -> bool {
    if imageinfo_sha1.len() == 31 {
        return imageinfo_sha1.eq_ignore_ascii_case(local_base36);
    }
    if imageinfo_sha1.len() != 40 || !imageinfo_sha1.is_ascii() {
        return false;
    }
    let mut digest = [0_u8; 20];
    for (index, byte) in digest.iter_mut().enumerate() {
        let start = index * 2;
        let Ok(value) = u8::from_str_radix(&imageinfo_sha1[start..start + 2], 16) else {
            return false;
        };
        *byte = value;
    }
    mediawiki_sha1_base36(&digest).eq_ignore_ascii_case(local_base36)
}

fn validate_commons_api_url(value: &str) -> Result<()> {
    let api_url = url::Url::parse(value).context("COMMONS_API_URL is not a valid URL")?;
    if !api_url.username().is_empty() || api_url.password().is_some() {
        bail!("COMMONS_API_URL must not contain credentials");
    }
    let loopback_http = api_url.scheme() == "http"
        && match api_url.host() {
            Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        };
    if api_url.scheme() != "https" && !loopback_http {
        bail!("COMMONS_API_URL must use HTTPS (HTTP is allowed only for loopback tests)");
    }
    Ok(())
}

fn encoded_commons_filename(filename: &str) -> String {
    url::form_urlencoded::byte_serialize(filename.replace(' ', "_").as_bytes()).collect()
}

fn file_page_url(filename: &str) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/File:{}",
        encoded_commons_filename(filename)
    )
}

fn file_redirect_url(filename: &str) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/Special:Redirect/file/{}",
        encoded_commons_filename(filename)
    )
}

pub fn build_wikitext(upload: &UploadRecord) -> String {
    let mut information = vec![
        "=={{int:filedesc}}==".to_string(),
        "{{Information".to_string(),
        "|description={{be|Фатаграфія для праекта [https://globustut.by/ «Глобус Беларусі»].}}"
            .to_string(),
        "|source=[https://globustut.by/ globustut.by]".to_string(),
    ];
    if let Some(author) = upload.author.as_deref() {
        information.push(format!(
            "|author=<nowiki>{}</nowiki>",
            html_escape::encode_text(author)
        ));
    }
    if let Some(date) = upload.capture_date.as_deref() {
        information.push(format!(
            "|date=<nowiki>{}</nowiki>",
            html_escape::encode_text(date)
        ));
    }
    information.push("}}".to_string());
    if let (Some(latitude), Some(longitude)) = (upload.latitude, upload.longitude) {
        information.push(format!("{{{{Location dec|{latitude:.6}|{longitude:.6}}}}}"));
    }
    information.extend([
        String::new(),
        "=={{int:license-header}}==".to_string(),
        "{{cc-by-4.0}}".to_string(),
        String::new(),
    ]);
    information.extend(
        upload
            .commons_categories
            .iter()
            .map(|category| format!("[[Category:{category}]]")),
    );
    information.join("\n")
}

fn validate_bot_password(password: &str) -> Result<()> {
    if password.is_empty() || password.len() > 1024 || password.chars().any(char::is_control) {
        bail!("the Commons BotPassword is empty or invalid");
    }
    Ok(())
}

pub fn store_bot_password(path: &Path, password: &str) -> Result<()> {
    validate_bot_password(password)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    file.write_all(password.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to protect {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_wikitext, file_page_url, file_redirect_url, imageinfo_sha1_matches,
        mediawiki_sha1_base36, validate_commons_api_url,
    };
    use crate::db::UploadRecord;
    use sha1::{Digest, Sha1};

    fn upload() -> UploadRecord {
        UploadRecord {
            id: "one".into(),
            stored_filename: "one.jpg".into(),
            original_filename: "one.jpg".into(),
            display_filename: "One photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            media_kind: "image".into(),
            size_bytes: 10,
            original_size_bytes: 10,
            converted: false,
            width: Some(1),
            height: Some(1),
            latitude: Some(41.636244),
            longitude: Some(41.613942),
            capture_date: Some("2026-07-03".into()),
            author: Some("A [[name]] | {{template}} </nowiki>".into()),
            commons_categories: vec!["Manors".into(), "Belarus".into()],
            license: "cc-by-4.0".into(),
            uploader_username: "anonymous".into(),
            created_at: 1,
        }
    }

    #[test]
    fn builds_fixed_license_wikitext_with_all_categories_and_safe_metadata() {
        let text = build_wikitext(&upload());
        assert!(text.contains("{{cc-by-4.0}}"));
        assert!(text.contains("[[Category:Manors]]\n[[Category:Belarus]]"));
        assert!(text.contains("{{Location dec|41.636244|41.613942}}"));
        assert!(text.contains("|date=<nowiki>2026-07-03</nowiki>"));
        assert!(text.contains("&lt;/nowiki&gt;"));
        assert!(!text.contains("{{self"));
    }

    #[test]
    fn omits_unknown_optional_metadata() {
        let mut upload = upload();
        upload.author = None;
        upload.capture_date = None;
        upload.latitude = None;
        upload.longitude = None;
        let text = build_wikitext(&upload);
        assert!(!text.contains("|author="));
        assert!(!text.contains("|date="));
        assert!(!text.contains("Location dec"));
    }

    #[test]
    fn encodes_commons_file_page_url() {
        assert_eq!(
            file_page_url("Новы файл & test.jpg"),
            "https://commons.wikimedia.org/wiki/File:%D0%9D%D0%BE%D0%B2%D1%8B_%D1%84%D0%B0%D0%B9%D0%BB_%26_test.jpg"
        );
        assert_eq!(
            file_redirect_url("Новы файл & test.jpg"),
            "https://commons.wikimedia.org/wiki/Special:Redirect/file/%D0%9D%D0%BE%D0%B2%D1%8B_%D1%84%D0%B0%D0%B9%D0%BB_%26_test.jpg"
        );
    }

    #[test]
    fn converts_sha1_to_mediawiki_base36() {
        assert_eq!(
            mediawiki_sha1_base36(&Sha1::digest([])),
            "phoiac9h4m842xq45sp7s6u21eteeq1"
        );
        assert_eq!(
            mediawiki_sha1_base36(&Sha1::digest(b"abc")),
            "jt72fo5t4yobf0qugwuczbwj07max7h"
        );
        let mut one = [0_u8; 20];
        one[19] = 1;
        assert_eq!(
            mediawiki_sha1_base36(&one),
            "0000000000000000000000000000001"
        );
        assert!(imageinfo_sha1_matches(
            "a9993e364706816aba3e25717850c26c9cd0d89d",
            "jt72fo5t4yobf0qugwuczbwj07max7h"
        ));
        assert!(imageinfo_sha1_matches(
            "JT72FO5T4YOBF0QUGWUCZBWJ07MAX7H",
            "jt72fo5t4yobf0qugwuczbwj07max7h"
        ));
        assert!(!imageinfo_sha1_matches(
            "not-a-valid-sha1",
            "jt72fo5t4yobf0qugwuczbwj07max7h"
        ));
    }

    #[test]
    fn only_allows_secure_or_loopback_commons_api_urls() {
        for allowed in [
            "https://commons.wikimedia.org/w/api.php",
            "http://localhost:8080/w/api.php",
            "http://127.0.0.1:8080/w/api.php",
            "http://[::1]:8080/w/api.php",
        ] {
            assert!(validate_commons_api_url(allowed).is_ok(), "{allowed}");
        }
        for rejected in [
            "http://commons.wikimedia.org/w/api.php",
            "ftp://commons.wikimedia.org/w/api.php",
            "https://user:password@commons.wikimedia.org/w/api.php",
            "not a URL",
        ] {
            assert!(validate_commons_api_url(rejected).is_err(), "{rejected}");
        }
    }
}
