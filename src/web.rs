use crate::auth::{AuthService, AuthenticatedAdmin};
use crate::commons::{CommonsClient, Coordinates, validate_category};
use crate::config::Config;
use crate::db::{Database, UploadRecord};
use crate::media::{MediaProcessor, renamed_display_filename, write_multipart_file};
use crate::templates::{self, SubmissionResult};
use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Form, Multipart, Path, Query, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, LOCATION,
    REFERRER_POLICY, SET_COOKIE, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::compression::CompressionLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

const ADMIN_PAGE_SIZE: usize = 40;
const MAX_SMALL_FIELD_BYTES: usize = 4 * 1024;
const MULTIPART_OVERHEAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Database,
    pub auth: AuthService,
    pub media: MediaProcessor,
    pub commons: CommonsClient,
    upload_slots: Arc<Semaphore>,
}

impl AppState {
    pub fn new(config: Arc<Config>, db: Database) -> Result<Self> {
        let auth = AuthService::new(&config, db.clone())?;
        let media = MediaProcessor::new(config.clone(), db.clone());
        let commons = CommonsClient::new(
            &config.user_agent,
            &config.commons_api_url,
            &config.wikidata_sparql_url,
            config.nearby_radius_meters,
            config.nearby_limit,
        )?;
        Ok(Self {
            config,
            db,
            auth,
            media,
            commons,
            upload_slots: Arc::new(Semaphore::new(1)),
        })
    }
}

pub fn router(state: AppState) -> Router {
    let upload_limit = state
        .config
        .max_request_bytes
        .saturating_add(MULTIPART_OVERHEAD_BYTES);
    let request_timeout = Duration::from_secs(state.config.request_timeout_seconds);
    let request_id_header = HeaderName::from_static("x-request-id");
    Router::new()
        .route("/", get(upload_page))
        .route(
            "/upload",
            post(upload).layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route("/admin", get(admin_page))
        .route("/admin/login", get(admin_login_page).post(admin_login))
        .route("/admin/logout", post(admin_logout))
        .route("/admin/media/{id}", get(admin_media))
        .route("/admin/thumbnail/{id}", get(admin_thumbnail))
        .route("/admin/uploads/{id}/rename", post(admin_rename))
        .route("/admin/uploads/{id}/category", post(admin_category))
        .route("/admin/uploads/{id}/delete", post(admin_delete))
        .route("/api/categories", get(category_search))
        .route(
            "/api/uploads/{id}/nearby-categories",
            get(nearby_categories),
        )
        .route("/healthz", get(health))
        .route("/static/app.css", get(static_css))
        .route("/static/app.js", get(static_js))
        .route("/static/favicon.svg", get(static_favicon))
        .fallback(not_found)
        .with_state(state)
        .layer(middleware::from_fn(security_headers))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .layer(CompressionLayer::new())
        .layer(CatchPanicLayer::new())
        .layer(PropagateRequestIdLayer::new(request_id_header.clone()))
        .layer(SetRequestIdLayer::new(request_id_header, MakeRequestUuid))
        .layer(TraceLayer::new_for_http())
}

#[derive(Debug, Deserialize, Default)]
struct UploadPageQuery {
    category: Option<String>,
}

async fn upload_page(
    State(state): State<AppState>,
    Query(query): Query<UploadPageQuery>,
) -> Html<String> {
    let category = query.category.as_deref().unwrap_or("");
    Html(templates::upload_page(
        category,
        state.config.max_files,
        state.config.max_file_bytes / (1024 * 1024),
        state.config.max_request_bytes / (1024 * 1024),
    ))
}

struct PendingFile {
    path: PathBuf,
    filename: String,
}

struct TemporaryFiles(Vec<PathBuf>);

impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            std::fs::remove_file(path).ok();
        }
    }
}

async fn upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let _permit = state
        .upload_slots
        .acquire()
        .await
        .map_err(|_| AppError::service_unavailable("Сервис загрузки останавливается."))?;
    let mut pending = Vec::new();
    let mut cleanup = TemporaryFiles(Vec::new());
    let mut category = None;
    let mut total_file_bytes = 0_u64;

    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| AppError::bad_request(format!("Некорректная форма: {error}")))?
    {
        match field.name().unwrap_or_default() {
            "files" => {
                if pending.len() >= state.config.max_files {
                    return Err(AppError::bad_request(format!(
                        "Можно отправить не более {} файлов за один раз.",
                        state.config.max_files
                    )));
                }
                let filename = field
                    .file_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| "unnamed-upload".into());
                let path = state
                    .config
                    .temp_dir
                    .join(format!("upload-{}", uuid::Uuid::new_v4()));
                cleanup.0.push(path.clone());
                let written = write_multipart_file(&mut field, &path, state.config.max_file_bytes)
                    .await
                    .map_err(|error| {
                        AppError::bad_request(format!("{}: {error}", safe_name(&filename)))
                    })?;
                total_file_bytes = total_file_bytes
                    .checked_add(written)
                    .ok_or_else(|| AppError::bad_request("Общий размер файлов слишком велик."))?;
                if total_file_bytes > state.config.max_request_bytes as u64 {
                    return Err(AppError::bad_request(format!(
                        "Общий размер файлов не должен превышать {} МБ.",
                        state.config.max_request_bytes / (1024 * 1024)
                    )));
                }
                pending.push(PendingFile { path, filename });
            }
            "category" => {
                category = Some(read_small_field(&mut field).await?);
            }
            // Old copies of the form may still submit these fields. The server always
            // assigns its fixed license, regardless of either legacy value.
            "license" | "rights_confirmed" => {
                let _ = read_small_field(&mut field).await?;
            }
            _ => return Err(AppError::bad_request("Форма содержит неизвестное поле.")),
        }
    }
    if pending.is_empty() {
        return Err(AppError::bad_request("Выберите хотя бы один файл."));
    }
    let category = category.unwrap_or_default();
    let mut results = Vec::with_capacity(pending.len());
    for file in pending {
        let result = state
            .media
            .ingest(&file.path, &file.filename, &category)
            .await;
        tokio::fs::remove_file(&file.path).await.ok();
        match result {
            Ok(record) => results.push(SubmissionResult {
                filename: record.display_filename,
                accepted: true,
                detail: if record.converted {
                    "Формат преобразован; файл ожидает проверки.".into()
                } else {
                    "Файл ожидает проверки.".into()
                },
            }),
            Err(error) => {
                tracing::warn!(filename = %safe_name(&file.filename), error = %format!("{error:#}"), "upload rejected");
                results.push(SubmissionResult {
                    filename: safe_name(&file.filename),
                    accepted: false,
                    detail: public_media_error(&error),
                });
            }
        }
    }
    drop(cleanup);
    Ok(no_store_html(templates::upload_results(&results)))
}

async fn read_small_field(
    field: &mut axum::extract::multipart::Field<'_>,
) -> Result<String, AppError> {
    let mut value = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|_| AppError::bad_request("Не удалось прочитать поле формы."))?
    {
        if value.len().saturating_add(chunk.len()) > MAX_SMALL_FIELD_BYTES {
            return Err(AppError::bad_request("Поле формы слишком длинное."));
        }
        value.extend_from_slice(&chunk);
    }
    String::from_utf8(value).map_err(|_| AppError::bad_request("Поле формы должно быть UTF-8."))
}

async fn admin_login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if state.auth.current_admin(&headers).await?.is_some() {
        return Ok(Redirect::to("/admin").into_response());
    }
    Ok(no_store_html(templates::login_page(None)))
}

#[derive(Debug, Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

async fn admin_login(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    match state.auth.login(&form.username, &form.password).await {
        Ok(login) => {
            tracing::info!(username = %login.username, "moderator logged in");
            let mut response = Redirect::to("/admin").into_response();
            response.headers_mut().insert(
                SET_COOKIE,
                HeaderValue::from_str(&login.set_cookie)
                    .map_err(|_| AppError::internal("failed to create session cookie"))?,
            );
            Ok(response)
        }
        Err(error) => {
            tracing::warn!(username = %safe_name(&form.username), error = %error, "moderator login failed");
            let mut response = no_store_html(templates::login_page(Some(
                "Неверный логин или пароль либо вход временно ограничен.",
            )));
            *response.status_mut() = StatusCode::UNAUTHORIZED;
            Ok(response)
        }
    }
}

#[derive(Debug, Deserialize)]
struct CsrfForm {
    csrf_token: String,
}

async fn admin_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let clear_cookie = state
        .auth
        .logout(&headers, &form.csrf_token)
        .await
        .map_err(|_| AppError::forbidden("Недействительный запрос выхода."))?;
    let mut response = Redirect::to("/admin/login").into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&clear_cookie)
            .map_err(|_| AppError::internal("failed to clear session cookie"))?,
    );
    Ok(response)
}

#[derive(Debug, Deserialize, Default)]
struct AdminQuery {
    page: Option<usize>,
}

async fn admin_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AdminQuery>,
) -> Result<Response, AppError> {
    let Some(admin) = state.auth.current_admin(&headers).await? else {
        return Ok(Redirect::to("/admin/login").into_response());
    };
    let page = query.page.unwrap_or(1).clamp(1, 10_000);
    let offset = (page - 1).saturating_mul(ADMIN_PAGE_SIZE);
    let mut uploads = state.db.list_uploads(ADMIN_PAGE_SIZE + 1, offset).await?;
    let has_next = uploads.len() > ADMIN_PAGE_SIZE;
    uploads.truncate(ADMIN_PAGE_SIZE);
    let (count, bytes) = state.db.upload_totals().await?;
    let disk = state.media.disk_space().await?;
    let html = templates::admin_page(templates::AdminPageData {
        username: &admin.username,
        csrf_token: &admin.csrf_token,
        uploads: &uploads,
        total_count: count,
        stored_bytes: bytes,
        disk: &disk,
        page_number: page,
        has_next_page: has_next,
    });
    Ok(no_store_html(html))
}

#[derive(Debug, Deserialize)]
struct RenameForm {
    csrf_token: String,
    basename: String,
}

async fn admin_rename(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Недействительный запрос."))?;
    let upload = required_upload(&state.db, &id).await?;
    let filename = renamed_display_filename(&form.basename, &upload.display_filename)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    state.db.update_display_filename(&id, &filename).await?;
    tracing::info!(admin = %admin.username, upload_id = %id, filename = %filename, "moderator renamed upload");
    Ok(redirect_to_upload(&id))
}

#[derive(Debug, Deserialize)]
struct CategoryForm {
    csrf_token: String,
    category: String,
}

async fn admin_category(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Недействительный запрос."))?;
    required_upload(&state.db, &id).await?;
    let category = validate_category(&form.category)
        .map_err(|_| AppError::bad_request("Некорректная категория Commons."))?;
    state.db.update_category(&id, &category).await?;
    tracing::info!(admin = %admin.username, upload_id = %id, category = %category, "moderator changed category");
    Ok(redirect_to_upload(&id))
}

async fn admin_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Недействительный запрос."))?;
    let upload = required_upload(&state.db, &id).await?;
    if !state.db.delete_upload(&id).await? {
        return Err(AppError::not_found("Материал не найден."));
    }
    if let Err(error) = state.media.delete_files(&upload).await {
        tracing::error!(admin = %admin.username, upload_id = %id, error = %format!("{error:#}"), "deleted database row but media cleanup must be retried");
    }
    tracing::info!(admin = %admin.username, upload_id = %id, "moderator deleted upload");
    Ok(Redirect::to("/admin").into_response())
}

async fn admin_media(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    require_admin(&state, &headers).await?;
    let upload = required_upload(&state.db, &id).await?;
    let path = state.media.stored_path(&upload)?;
    stream_private_file(path, &upload.mime_type).await
}

async fn admin_thumbnail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    require_admin(&state, &headers).await?;
    let upload = required_upload(&state.db, &id).await?;
    if upload.media_kind != "image" {
        return Err(AppError::not_found("Миниатюра не найдена."));
    }
    stream_private_file(state.media.thumbnail_path(&id), "image/webp").await
}

async fn stream_private_file(path: PathBuf, mime_type: &str) -> Result<Response, AppError> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AppError::not_found("Файл не найден."),
            _ => AppError::internal(format!("failed to open {}: {error}", path.display())),
        })?;
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime_type)
            .map_err(|_| AppError::internal("invalid stored MIME type"))?,
    );
    headers.insert(CONTENT_DISPOSITION, HeaderValue::from_static("inline"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(response)
}

#[derive(Debug, Deserialize)]
struct CategorySearchQuery {
    q: Option<String>,
}

#[derive(Debug, Serialize)]
struct CategorySearchResponse {
    categories: Vec<String>,
}

async fn category_search(
    State(state): State<AppState>,
    Query(query): Query<CategorySearchQuery>,
) -> Result<Json<CategorySearchResponse>, AppError> {
    let query = query.q.unwrap_or_default();
    if query.len() > 255 {
        return Err(AppError::bad_request("Слишком длинный поисковый запрос."));
    }
    let categories = state.commons.search_categories(&query).await?;
    Ok(Json(CategorySearchResponse { categories }))
}

#[derive(Debug, Serialize)]
struct NearbyResponse {
    categories: Vec<crate::commons::NearbyCategory>,
}

async fn nearby_categories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<NearbyResponse>, AppError> {
    require_admin(&state, &headers).await?;
    let upload = required_upload(&state.db, &id).await?;
    let coordinates = Coordinates::new(
        upload.latitude.context("В EXIF нет широты.")?,
        upload.longitude.context("В EXIF нет долготы.")?,
    )
    .context("Координаты EXIF некорректны.")?;
    let categories = state.commons.nearby_categories(coordinates).await?;
    Ok(Json(NearbyResponse { categories }))
}

async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedAdmin, AppError> {
    state
        .auth
        .current_admin(headers)
        .await?
        .ok_or_else(|| AppError::unauthorized("Требуется вход в премодерацию."))
}

async fn required_upload(db: &Database, id: &str) -> Result<UploadRecord, AppError> {
    db.get_upload(id)
        .await?
        .ok_or_else(|| AppError::not_found("Материал не найден."))
}

fn redirect_to_upload(id: &str) -> Response {
    let location = format!("/admin#upload-{id}");
    let mut response = StatusCode::SEE_OTHER.into_response();
    if let Ok(value) = HeaderValue::from_str(&location) {
        response.headers_mut().insert(LOCATION, value);
    }
    response
}

async fn health(State(state): State<AppState>) -> Response {
    match state.db.upload_totals().await {
        Ok(_) => (StatusCode::OK, "ok\n").into_response(),
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "health check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "database unavailable\n").into_response()
        }
    }
}

async fn static_css() -> Response {
    static_response(
        "text/css; charset=utf-8",
        include_bytes!("../static/app.css"),
    )
}

async fn static_js() -> Response {
    static_response(
        "text/javascript; charset=utf-8",
        include_bytes!("../static/app.js"),
    )
}

async fn static_favicon() -> Response {
    static_response("image/svg+xml", include_bytes!("../static/favicon.svg"))
}

fn static_response(content_type: &'static str, bytes: &'static [u8]) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    response
}

async fn not_found() -> AppError {
    AppError::not_found("Страница не найдена.")
}

async fn security_headers(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; img-src 'self' data:; media-src 'self'; object-src 'none'; script-src 'self'; style-src 'self'",
        ),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), geolocation=(), microphone=()"),
    );
    response
}

fn no_store_html(html: String) -> Response {
    let mut response = Html(html).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response.headers_mut().insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow"),
    );
    response
}

fn safe_name(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .take(200)
        .collect::<String>()
}

fn public_media_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if message.contains("larger than") {
        "Файл превышает допустимый размер.".into()
    } else if message.contains("empty") {
        "Получен пустой файл.".into()
    } else if message.contains("unsupported") {
        "Формат файла не поддерживается либо файл повреждён.".into()
    } else if message.contains("dimensions") {
        "Размеры изображения превышают допустимые.".into()
    } else if message.contains("storage") {
        "На сервере недостаточно свободного места.".into()
    } else if message.contains("queue") {
        "Очередь премодерации заполнена. Попробуйте позже.".into()
    } else if message.contains("category") {
        "Укажите категорию Wikimedia Commons.".into()
    } else if message.contains("conversion") {
        "Не удалось преобразовать формат изображения.".into()
    } else {
        "Файл повреждён либо его формат пока не поддерживается.".into()
    }
}

#[derive(Debug)]
pub struct AppError {
    status: StatusCode,
    public_message: String,
}

impl AppError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            public_message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            public_message: message.into(),
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            public_message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            public_message: message.into(),
        }
    }

    fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            public_message: message.into(),
        }
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        tracing::error!(error = %message, "request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public_message: "Внутренняя ошибка сервера.".into(),
        }
    }
}

impl From<anyhow::Error> for AppError {
    fn from(error: anyhow::Error) -> Self {
        Self::internal(format!("{error:#}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let html = templates::error_page(self.status.as_u16(), &self.public_message);
        (self.status, Html(html)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::public_media_error;

    #[test]
    fn internal_media_errors_are_not_exposed() {
        let internal = anyhow::anyhow!("failed at /secret/path/image.jpg");
        assert!(!public_media_error(&internal).contains("/secret/path"));
        let expected = anyhow::anyhow!("unsupported or invalid media file");
        assert!(public_media_error(&expected).contains("не поддерживается"));
    }
}
