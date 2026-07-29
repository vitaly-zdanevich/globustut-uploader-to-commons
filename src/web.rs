use crate::auth::{AuthService, AuthenticatedAdmin};
use crate::commons::{CommonsClient, Coordinates, validate_categories, validate_category};
use crate::config::Config;
use crate::db::{Database, UploadRecord};
use crate::media::{MediaProcessor, renamed_display_filename, write_multipart_file};
use crate::publisher::{COMMONS_ACCOUNT, CommonsPublisher};
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
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
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
    pub publisher: Option<CommonsPublisher>,
    upload_slots: Arc<Semaphore>,
    upload_operations: Arc<StdMutex<HashSet<String>>>,
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
        let publisher = CommonsPublisher::from_config(&config)?;
        Ok(Self {
            config,
            db,
            auth,
            media,
            commons,
            publisher,
            upload_slots: Arc::new(Semaphore::new(1)),
            upload_operations: Arc::new(StdMutex::new(HashSet::new())),
        })
    }

    fn claim_upload(&self, id: &str) -> Result<UploadOperationGuard, AppError> {
        let mut operations = self
            .upload_operations
            .lock()
            .map_err(|_| AppError::internal("upload operation lock is poisoned"))?;
        if !operations.insert(id.to_string()) {
            return Err(AppError::locked(
                "З гэтым файлам ужо выконваецца іншая аперацыя.",
            ));
        }
        Ok(UploadOperationGuard {
            id: id.to_string(),
            operations: self.upload_operations.clone(),
        })
    }
}

struct UploadOperationGuard {
    id: String,
    operations: Arc<StdMutex<HashSet<String>>>,
}

impl Drop for UploadOperationGuard {
    fn drop(&mut self) {
        if let Ok(mut operations) = self.operations.lock() {
            operations.remove(&self.id);
        }
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
        .route("/admin/uploads/{id}/categories", post(admin_category_add))
        .route(
            "/admin/uploads/{id}/categories/remove",
            post(admin_category_remove),
        )
        .route("/admin/uploads/{id}/category", post(admin_category_replace))
        .route("/admin/publish", post(admin_publish))
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
        .map_err(|_| AppError::service_unavailable("Сэрвіс запампоўвання спыняецца."))?;
    let mut pending = Vec::new();
    let mut cleanup = TemporaryFiles(Vec::new());
    let mut category = None;
    let mut total_file_bytes = 0_u64;

    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        tracing::warn!(error = %error, "invalid multipart form");
        AppError::bad_request("Некарэктная форма.")
    })? {
        match field.name().unwrap_or_default() {
            "files" => {
                if pending.len() >= state.config.max_files {
                    return Err(AppError::bad_request(format!(
                        "За адзін раз можна адправіць не больш за {} файлаў.",
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
                        let filename = safe_name(&filename);
                        tracing::warn!(filename = %filename, error = %format!("{error:#}"), "failed to buffer multipart file");
                        if error.to_string().contains("exceeds the configured size limit") {
                            AppError::bad_request(format!(
                                "Файл «{filename}» перавышае дапушчальны памер."
                            ))
                        } else {
                            AppError::bad_request(format!(
                                "Не ўдалося прыняць файл «{filename}»."
                            ))
                        }
                    })?;
                total_file_bytes = total_file_bytes
                    .checked_add(written)
                    .ok_or_else(|| AppError::bad_request("Агульны памер файлаў занадта вялікі."))?;
                if total_file_bytes > state.config.max_request_bytes as u64 {
                    return Err(AppError::bad_request(format!(
                        "Агульны памер файлаў не павінен перавышаць {} МБ.",
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
            _ => return Err(AppError::bad_request("Форма змяшчае невядомае поле.")),
        }
    }
    if pending.is_empty() {
        return Err(AppError::bad_request("Выберыце хаця б адзін файл."));
    }
    let category = category.unwrap_or_default();
    let categories = [category];
    let mut results = Vec::with_capacity(pending.len());
    for file in pending {
        let result = state
            .media
            .ingest(&file.path, &file.filename, &categories)
            .await;
        tokio::fs::remove_file(&file.path).await.ok();
        match result {
            Ok(record) => results.push(SubmissionResult {
                filename: record.display_filename,
                accepted: true,
                detail: if record.converted {
                    "Фармат пераўтвораны; файл чакае праверкі.".into()
                } else {
                    "Файл чакае праверкі.".into()
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
        .map_err(|_| AppError::bad_request("Не ўдалося прачытаць поле формы."))?
    {
        if value.len().saturating_add(chunk.len()) > MAX_SMALL_FIELD_BYTES {
            return Err(AppError::bad_request("Поле формы занадта доўгае."));
        }
        value.extend_from_slice(&chunk);
    }
    String::from_utf8(value).map_err(|_| AppError::bad_request("Поле формы павінна быць у UTF-8."))
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
                "Няправільнае імя карыстальніка або пароль, альбо ўваход часова абмежаваны.",
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
        .map_err(|_| AppError::forbidden("Несапраўдны запыт на выхад."))?;
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
    let html = templates::admin_page(templates::AdminPageData {
        username: &admin.username,
        csrf_token: &admin.csrf_token,
        commons_account: Some(COMMONS_ACCOUNT),
        uploads: &uploads,
        total_count: count,
        stored_bytes: bytes,
        storage_limit_bytes: state.config.max_pending_bytes,
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
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&id)?;
    let upload = required_upload(&state.db, &id).await?;
    let filename =
        renamed_display_filename(&form.basename, &upload.display_filename).map_err(|error| {
            if error.to_string().contains("filename must not be empty") {
                AppError::bad_request("Імя файла не можа быць пустым.")
            } else {
                AppError::internal(format!("failed to rename upload {}: {error:#}", upload.id))
            }
        })?;
    state.db.update_display_filename(&id, &filename).await?;
    tracing::info!(admin = %admin.username, upload_id = %id, filename = %filename, "moderator renamed upload");
    Ok(redirect_to_upload(&id))
}

#[derive(Debug, Deserialize)]
struct CategoryForm {
    csrf_token: String,
    category: String,
}

#[derive(Debug, Serialize)]
struct CategoryUpdateResponse {
    categories: Vec<String>,
}

async fn admin_category_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&id)?;
    let category = validate_category(&form.category)
        .map_err(|_| AppError::bad_request("Некарэктная катэгорыя Commons."))?;
    let mut categories = required_upload(&state.db, &id).await?.commons_categories;
    if !categories.iter().any(|existing| existing == &category) {
        categories.push(category.clone());
        categories = validate_categories(&categories)
            .map_err(|_| AppError::bad_request("Некарэктны спіс катэгорый Commons."))?;
        state.db.replace_categories(&id, &categories).await?;
        tracing::info!(admin = %admin.username, upload_id = %id, category = %category, "moderator added category");
    }
    category_update_response(&headers, &id, categories)
}

async fn admin_category_remove(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&id)?;
    let category = validate_category(&form.category)
        .map_err(|_| AppError::bad_request("Некарэктная катэгорыя Commons."))?;
    let mut categories = required_upload(&state.db, &id).await?.commons_categories;
    if categories.len() == 1 && categories.first() == Some(&category) {
        return Err(AppError::bad_request(
            "Файл павінен мець хаця б адну катэгорыю. Спачатку дадайце новую.",
        ));
    }
    let original_len = categories.len();
    categories.retain(|existing| existing != &category);
    if categories.len() != original_len {
        state.db.replace_categories(&id, &categories).await?;
        tracing::info!(admin = %admin.username, upload_id = %id, category = %category, "moderator removed category");
    }
    category_update_response(&headers, &id, categories)
}

async fn admin_category_replace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CategoryForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&id)?;
    required_upload(&state.db, &id).await?;
    let category = validate_category(&form.category)
        .map_err(|_| AppError::bad_request("Некарэктная катэгорыя Commons."))?;
    let categories = vec![category.clone()];
    state.db.replace_categories(&id, &categories).await?;
    tracing::info!(admin = %admin.username, upload_id = %id, category = %category, "moderator replaced categories through legacy route");
    category_update_response(&headers, &id, categories)
}

fn category_update_response(
    headers: &HeaderMap,
    id: &str,
    categories: Vec<String>,
) -> Result<Response, AppError> {
    if accepts_json(headers) {
        Ok(no_store_json(CategoryUpdateResponse { categories }))
    } else {
        Ok(redirect_to_upload(id))
    }
}

#[derive(Debug, Deserialize)]
struct PublishForm {
    csrf_token: String,
    upload_id: String,
}

#[derive(Debug, Serialize)]
struct PublishResponse {
    upload_id: String,
    display_filename: String,
    filename: String,
    page_url: String,
    file_url: Option<String>,
}

async fn admin_publish(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<PublishForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&form.upload_id)?;
    let publisher = state.publisher.as_ref().ok_or_else(|| {
        AppError::service_unavailable("Уліковы запіс Wikimedia Commons яшчэ не наладжаны.")
    })?;
    let upload = required_upload(&state.db, &form.upload_id).await?;
    validate_categories(&upload.commons_categories)
        .map_err(|_| AppError::bad_request("Файл павінен мець хаця б адну катэгорыю."))?;
    let stored_path = state.media.stored_path(&upload)?;
    let published = publisher
        .publish(&upload, &stored_path)
        .await
        .map_err(|error| publish_error(&admin, &upload, error))?;

    if !state.db.delete_upload(&upload.id).await? {
        return Err(AppError::not_found("Матэрыял не знойдзены."));
    }
    if let Err(error) = state.media.delete_files(&upload).await {
        tracing::error!(
            admin = %admin.username,
            upload_id = %upload.id,
            error = %format!("{error:#}"),
            "published upload was removed from the database but media cleanup must be retried"
        );
    }
    tracing::info!(
        admin = %admin.username,
        upload_id = %upload.id,
        commons_filename = %published.filename,
        "moderator published upload to Wikimedia Commons"
    );
    let response = PublishResponse {
        upload_id: upload.id,
        display_filename: upload.display_filename,
        filename: published.filename,
        page_url: published.page_url,
        file_url: published.file_url,
    };
    if accepts_json(&headers) {
        Ok(no_store_json(response))
    } else {
        Ok(Redirect::to("/admin").into_response())
    }
}

fn publish_error(
    admin: &AuthenticatedAdmin,
    upload: &UploadRecord,
    error: anyhow::Error,
) -> AppError {
    let detail = format!("{error:#}");
    tracing::error!(
        admin = %admin.username,
        upload_id = %upload.id,
        error = %detail,
        "Wikimedia Commons publication failed"
    );
    if detail.contains("already exists") {
        AppError::conflict(
            "Файл з такой назвай ужо існуе ў Wikimedia Commons. Спачатку змяніце імя.",
        )
    } else if detail.contains("login") || detail.contains("authentication") {
        AppError::service_unavailable("Не ўдалося ўвайсці ва ўліковы запіс Wikimedia Commons.")
    } else if detail.contains("ratelimit") || detail.contains("rate limit") {
        AppError::service_unavailable(
            "Сэрвіс Wikimedia Commons часова абмежаваў частату запытаў. Паўтарыце пазней.",
        )
    } else {
        AppError::bad_gateway("Не ўдалося запампаваць файл у Wikimedia Commons.")
    }
}

async fn admin_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let admin = require_admin(&state, &headers).await?;
    AuthService::validate_csrf(&admin, &form.csrf_token)
        .map_err(|_| AppError::forbidden("Несапраўдны запыт."))?;
    let _operation = state.claim_upload(&id)?;
    let upload = required_upload(&state.db, &id).await?;
    if !state.db.delete_upload(&id).await? {
        return Err(AppError::not_found("Матэрыял не знойдзены."));
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
        return Err(AppError::not_found("Мініяцюра не знойдзена."));
    }
    stream_private_file(state.media.thumbnail_path(&id), "image/webp").await
}

async fn stream_private_file(path: PathBuf, mime_type: &str) -> Result<Response, AppError> {
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => AppError::not_found("Файл не знойдзены."),
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
        return Err(AppError::bad_request("Пошукавы запыт занадта доўгі."));
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
    .context("Каардынаты EXIF некарэктныя.")?;
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
        .ok_or_else(|| AppError::unauthorized("Патрабуецца ўвайсці ў панэль прэмадэрацыі."))
}

async fn required_upload(db: &Database, id: &str) -> Result<UploadRecord, AppError> {
    db.get_upload(id)
        .await?
        .ok_or_else(|| AppError::not_found("Матэрыял не знойдзены."))
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
    AppError::not_found("Старонка не знойдзена.")
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

fn no_store_json(value: impl Serialize) -> Response {
    let mut response = Json(value).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    response
}

fn accepts_json(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|item| item.trim() == "application/json")
        })
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
        "Файл перавышае дапушчальны памер.".into()
    } else if message.contains("empty") {
        "Атрыманы пусты файл.".into()
    } else if message.contains("unsupported") {
        "Фармат файла не падтрымліваецца або файл пашкоджаны.".into()
    } else if message.contains("dimensions") {
        "Памеры выявы перавышаюць дапушчальныя.".into()
    } else if message.contains("storage") {
        "На серверы недастаткова вольнага месца.".into()
    } else if message.contains("queue") {
        "Чарга прэмадэрацыі запоўненая. Паспрабуйце пазней.".into()
    } else if message.contains("category") {
        "Пазначце хаця б адну катэгорыю Wikimedia Commons.".into()
    } else if message.contains("conversion") {
        "Не ўдалося пераўтварыць фармат выявы.".into()
    } else {
        "Файл пашкоджаны або яго фармат пакуль не падтрымліваецца.".into()
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

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            public_message: message.into(),
        }
    }

    fn locked(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::LOCKED,
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

    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            public_message: message.into(),
        }
    }

    fn internal(message: impl std::fmt::Display) -> Self {
        tracing::error!(error = %message, "request failed");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            public_message: "Унутраная памылка сервера.".into(),
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
        assert!(public_media_error(&expected).contains("не падтрымліваецца"));
    }
}
