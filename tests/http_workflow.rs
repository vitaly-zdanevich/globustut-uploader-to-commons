mod support;

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use globustut_uploader_to_commons::auth::{hash_password, now_ts};
use globustut_uploader_to_commons::db::{Database, UploadRecord};
use globustut_uploader_to_commons::media::FIXED_UPLOAD_LICENSE;
use globustut_uploader_to_commons::publisher::store_bot_password;
use globustut_uploader_to_commons::web::{AppState, router};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const ADMIN_PASSWORD: &str = "correct horse battery staple";
const MOCK_BOT_PASSWORD: &str = "mock Commons BotPassword";
const MOCK_LOGIN_COOKIE: &str = "mock_login=issued";
const MOCK_SESSION_COOKIE: &str = "mock_session=authenticated";

#[tokio::test]
async fn upload_login_rename_categories_and_delete_workflow() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    create_moderator(&db).await;
    let app = router(AppState::new(config.clone(), db.clone()).unwrap());

    let source = config.temp_dir.join("fixture.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "80x60", "xc:#4cb6ac"])
            .arg(&source),
    );
    let jpeg = std::fs::read(source).unwrap();
    let boundary = "globustut-test-boundary";
    let multipart = multipart_upload(
        boundary,
        "castle.jpg",
        &jpeg,
        &["Category:Castles_in_Belarus"],
    );
    let response = app
        .clone()
        .oneshot(
            Request::post("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(multipart))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("content-security-policy"));
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let html = body_text(response).await;
    assert!(html.contains("Прынята файлаў: 1"));

    let upload = db.list_uploads(10, 0).await.unwrap().pop().unwrap();
    assert_eq!(upload.commons_categories, ["Castles in Belarus"]);
    assert_eq!(upload.display_filename, "castle.jpg");
    assert_eq!(upload.license, FIXED_UPLOAD_LICENSE);

    let response = app
        .clone()
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/admin/login");

    let cookie = login(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::get("/admin")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let admin_html = body_text(response).await;
    assert!(admin_html.contains("Прэмадэрацыя"));
    assert!(admin_html.contains("castle.jpg"));
    assert!(admin_html.contains("Castles in Belarus"));
    let csrf = hidden_value(&admin_html, "csrf_token");

    let rename = form_body(&[
        ("csrf_token", csrf.as_str()),
        ("basename", "Нясвіжскі замак"),
    ]);
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/admin/uploads/{}/rename", upload.id))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(rename))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        db.get_upload(&upload.id)
            .await
            .unwrap()
            .unwrap()
            .display_filename,
        "Нясвіжскі замак.jpg"
    );

    let response = category_request(&app, &cookie, &upload.id, "/categories", &csrf, "Minsk").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["categories"],
        json!(["Castles in Belarus", "Minsk"])
    );

    let response = category_request(
        &app,
        &cookie,
        &upload.id,
        "/categories",
        &csrf,
        "Category:Castles_of_Belarus",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    assert_eq!(
        body_json(response).await["categories"],
        json!(["Castles in Belarus", "Minsk", "Castles of Belarus"])
    );

    let duplicate = category_request(
        &app,
        &cookie,
        &upload.id,
        "/categories",
        &csrf,
        "Castles_of_Belarus",
    )
    .await;
    assert_eq!(duplicate.status(), StatusCode::OK);
    assert_eq!(
        body_json(duplicate).await["categories"],
        json!(["Castles in Belarus", "Minsk", "Castles of Belarus"])
    );

    let response = category_request(
        &app,
        &cookie,
        &upload.id,
        "/categories/remove",
        &csrf,
        "Castles in Belarus",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["categories"],
        json!(["Minsk", "Castles of Belarus"])
    );
    assert_eq!(
        db.get_upload(&upload.id)
            .await
            .unwrap()
            .unwrap()
            .commons_categories,
        ["Minsk", "Castles of Belarus"]
    );

    let response = category_request(
        &app,
        &cookie,
        &upload.id,
        "/categories/remove",
        &csrf,
        "Minsk",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["categories"],
        json!(["Castles of Belarus"])
    );

    let response = category_request(
        &app,
        &cookie,
        &upload.id,
        "/categories/remove",
        &csrf,
        "Castles of Belarus",
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_text(response)
            .await
            .contains("Файл павінен мець хаця б адну катэгорыю")
    );
    assert_eq!(
        db.get_upload(&upload.id)
            .await
            .unwrap()
            .unwrap()
            .commons_categories,
        ["Castles of Belarus"]
    );

    let delete = form_body(&[("csrf_token", csrf.as_str())]);
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/admin/uploads/{}/delete", upload.id))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(delete))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(db.get_upload(&upload.id).await.unwrap().is_none());
    assert!(!config.upload_dir.join(upload.stored_filename).exists());
}

#[tokio::test]
async fn publishes_moderated_upload_to_commons_and_cleans_pending_files() {
    let mock = MockCommonsServer::spawn().await;
    {
        let mut behavior = mock.state.behavior.lock().unwrap();
        behavior.omit_final_imageinfo = true;
        behavior.fail_url_enrichment = true;
    }
    let root = tempfile::tempdir().unwrap();
    let mut config = support::test_config(root.path());
    config.commons_api_url = mock.api_url.clone();
    config.prepare_directories().unwrap();
    store_bot_password(&config.commons_bot_password_path, MOCK_BOT_PASSWORD).unwrap();
    let config = Arc::new(config);
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    create_moderator(&db).await;
    let app = router(AppState::new(config.clone(), db.clone()).unwrap());

    let source = config.temp_dir.join("commons-fixture.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "96x64", "xc:#4cb6ac"])
            .arg(&source),
    );
    let jpeg = std::fs::read(source).unwrap();
    let boundary = "globustut-publish-boundary";
    let response = app
        .clone()
        .oneshot(
            Request::post("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(multipart_upload(
                    boundary,
                    "commons-castle.jpg",
                    &jpeg,
                    &["Category:Manors_in_Belarus"],
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let upload = db.list_uploads(10, 0).await.unwrap().pop().unwrap();
    let stored_path = config.upload_dir.join(&upload.stored_filename);
    let thumbnail_path = config.thumbnail_dir.join(format!("{}.webp", upload.id));
    assert!(stored_path.is_file());
    assert!(thumbnail_path.is_file());

    let cookie = login(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::get("/admin")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let admin_html = body_text(response).await;
    assert_eq!(status, StatusCode::OK, "{admin_html}");
    let csrf = hidden_value(&admin_html, "csrf_token");
    let response =
        category_request(&app, &cookie, &upload.id, "/categories", &csrf, "Belarus").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["categories"],
        json!(["Manors in Belarus", "Belarus"])
    );
    let publish = form_body(&[
        ("csrf_token", csrf.as_str()),
        ("upload_id", upload.id.as_str()),
    ]);
    let response = app
        .oneshot(
            Request::post("/admin/publish")
                .header(header::COOKIE, &cookie)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(publish))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, no-store"
    );
    let payload = body_json(response).await;
    assert_eq!(payload["upload_id"], upload.id);
    assert_eq!(payload["display_filename"], "commons-castle.jpg");
    assert_eq!(payload["filename"], "commons-castle.jpg");
    assert_eq!(
        payload["page_url"],
        "https://commons.wikimedia.org/wiki/File:commons-castle.jpg"
    );
    assert_eq!(
        payload["file_url"],
        "https://commons.wikimedia.org/wiki/Special:Redirect/file/commons-castle.jpg"
    );

    assert!(db.get_upload(&upload.id).await.unwrap().is_none());
    assert!(!stored_path.exists());
    assert!(!thumbnail_path.exists());

    let calls = mock.state.calls.lock().unwrap();
    assert_eq!(calls.login_attempts, 1);
    assert_eq!(calls.title_checks, vec!["commons-castle.jpg"]);
    assert_eq!(calls.url_enrichments, 1);
    assert_eq!(calls.chunk_uploads, 1);
    assert_eq!(calls.final_uploads.len(), 1);
    let published = &calls.final_uploads[0];
    assert_eq!(published.filename, "commons-castle.jpg");
    assert_eq!(
        published.comment,
        "Запампавана пасля прэмадэрацыі на globustut.by"
    );
    assert!(published.text.contains("{{cc-by-4.0}}"));
    assert!(published.text.contains("[[Category:Manors in Belarus]]"));
    assert!(published.text.contains("[[Category:Belarus]]"));
}

#[tokio::test]
async fn reconciles_an_ambiguous_final_commit_and_cleans_pending_files() {
    let mock = MockCommonsServer::spawn().await;
    let fixture = PendingPublishFixture::create(&mock, "ambiguous-castle.jpg", "#136f63").await;
    let sha1 = sha1_hex_for_test(&std::fs::read(&fixture.stored_path).unwrap());
    {
        let mut behavior = mock.state.behavior.lock().unwrap();
        behavior.ambiguous_final_file =
            Some(mock_existing_file(&fixture.upload, sha1, "Globustut"));
        behavior.ambiguous_visibility_misses = 1;
    }

    let response = publish_request(
        &fixture.app,
        &fixture.cookie,
        &fixture.csrf,
        &fixture.upload.id,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["filename"], "ambiguous-castle.jpg");
    assert_eq!(
        payload["page_url"],
        "https://commons.wikimedia.org/wiki/File:ambiguous-castle.jpg"
    );
    assert_eq!(
        payload["file_url"],
        "https://upload.wikimedia.org/mock/ambiguous-castle.jpg"
    );
    assert!(
        fixture
            .db
            .get_upload(&fixture.upload.id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!fixture.stored_path.exists());
    assert!(!fixture.thumbnail_path.exists());

    let calls = mock.state.calls.lock().unwrap();
    assert_eq!(
        calls.title_checks,
        vec![
            "ambiguous-castle.jpg",
            "ambiguous-castle.jpg",
            "ambiguous-castle.jpg"
        ]
    );
    assert_eq!(calls.chunk_uploads, 1);
    assert_eq!(calls.final_uploads.len(), 1);
}

#[tokio::test]
async fn preserves_pending_upload_when_ambiguous_commit_does_not_match() {
    let mock = MockCommonsServer::spawn().await;
    let fixture = PendingPublishFixture::create(&mock, "mismatch-castle.jpg", "#b23a48").await;
    let sha1 = sha1_hex_for_test(&std::fs::read(&fixture.stored_path).unwrap());
    {
        let mut existing = mock_existing_file(&fixture.upload, sha1, "Globustut");
        existing.comment = "A different upload comment".into();
        mock.state.behavior.lock().unwrap().ambiguous_final_file = Some(existing);
    }

    let response = publish_request(
        &fixture.app,
        &fixture.cookie,
        &fixture.csrf,
        &fixture.upload.id,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(
        fixture
            .db
            .get_upload(&fixture.upload.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(fixture.stored_path.is_file());
    assert!(fixture.thumbnail_path.is_file());

    let calls = mock.state.calls.lock().unwrap();
    assert_eq!(
        calls.title_checks,
        vec!["mismatch-castle.jpg", "mismatch-castle.jpg"]
    );
    assert_eq!(calls.chunk_uploads, 1);
    assert_eq!(calls.final_uploads.len(), 1);
}

#[tokio::test]
async fn reconciles_only_an_exact_previous_commons_upload_before_cleanup() {
    let mock = MockCommonsServer::spawn().await;
    let root = tempfile::tempdir().unwrap();
    let mut config = support::test_config(root.path());
    config.commons_api_url = mock.api_url.clone();
    config.prepare_directories().unwrap();
    store_bot_password(&config.commons_bot_password_path, MOCK_BOT_PASSWORD).unwrap();
    let config = Arc::new(config);
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    create_moderator(&db).await;
    let app = router(AppState::new(config.clone(), db.clone()).unwrap());

    let source = config.temp_dir.join("retry-fixture.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "72x48", "xc:#c42c38"])
            .arg(&source),
    );
    let jpeg = std::fs::read(source).unwrap();
    let boundary = "globustut-reconcile-boundary";
    let response = app
        .clone()
        .oneshot(
            Request::post("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(multipart_upload(
                    boundary,
                    "retry-castle.jpg",
                    &jpeg,
                    &["Category:Castles_in_Belarus"],
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let upload = db.list_uploads(10, 0).await.unwrap().pop().unwrap();
    let stored_path = config.upload_dir.join(&upload.stored_filename);
    let thumbnail_path = config.thumbnail_dir.join(format!("{}.webp", upload.id));
    let commons_sha1 = sha1_hex_for_test(&std::fs::read(&stored_path).unwrap());
    let existing = MockExistingFile {
        filename: upload.display_filename.clone(),
        sha1: "0000000000000000000000000000000".into(),
        uploader: "Globustut".into(),
        comment: "Запампавана пасля прэмадэрацыі на globustut.by".into(),
        page_url: "https://commons.wikimedia.org/wiki/File:retry-castle.jpg".into(),
        file_url: "https://upload.wikimedia.org/mock/retry-castle.jpg".into(),
    };
    mock.state.behavior.lock().unwrap().existing_file = Some(existing);

    let cookie = login(&app).await;
    let response = app
        .clone()
        .oneshot(
            Request::get("/admin")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let csrf = hidden_value(&body_text(response).await, "csrf_token");

    let response = publish_request(&app, &cookie, &csrf, &upload.id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(db.get_upload(&upload.id).await.unwrap().is_some());
    assert!(stored_path.is_file());

    {
        let mut behavior = mock.state.behavior.lock().unwrap();
        let existing = behavior.existing_file.as_mut().unwrap();
        existing.sha1 = commons_sha1.clone();
        existing.uploader = "Different user".into();
    }
    let response = publish_request(&app, &cookie, &csrf, &upload.id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    {
        let mut behavior = mock.state.behavior.lock().unwrap();
        let existing = behavior.existing_file.as_mut().unwrap();
        existing.uploader = "Globustut".into();
        existing.comment = "Different upload comment".into();
    }
    let response = publish_request(&app, &cookie, &csrf, &upload.id).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    mock.state
        .behavior
        .lock()
        .unwrap()
        .existing_file
        .as_mut()
        .unwrap()
        .comment = "Запампавана пасля прэмадэрацыі на globustut.by".into();
    let response = publish_request(&app, &cookie, &csrf, &upload.id).await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(
        payload["page_url"],
        "https://commons.wikimedia.org/wiki/File:retry-castle.jpg"
    );
    assert_eq!(
        payload["file_url"],
        "https://upload.wikimedia.org/mock/retry-castle.jpg"
    );
    assert!(db.get_upload(&upload.id).await.unwrap().is_none());
    assert!(!stored_path.exists());
    assert!(!thumbnail_path.exists());

    let calls = mock.state.calls.lock().unwrap();
    assert_eq!(calls.login_attempts, 1);
    assert_eq!(calls.title_checks, vec!["retry-castle.jpg"; 4]);
    assert_eq!(calls.chunk_uploads, 0);
    assert!(calls.final_uploads.is_empty());
}

#[tokio::test]
async fn rejects_excess_combined_file_payload_and_cleans_buffers() {
    let root = tempfile::tempdir().unwrap();
    let mut config = support::test_config(root.path());
    config.max_file_bytes = 700 * 1024;
    config.max_request_bytes = 1024 * 1024;
    config.max_files = 2;
    let config = Arc::new(config);
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    let app = router(AppState::new(config.clone(), db.clone()).unwrap());

    let first = vec![b'a'; 600 * 1024];
    let second = vec![b'b'; 600 * 1024];
    let boundary = "globustut-size-boundary";
    let multipart = multipart_upload_many(
        boundary,
        &[
            ("first.jpg", first.as_slice()),
            ("second.jpg", second.as_slice()),
        ],
        &["Category:Castles_in_Belarus"],
    );
    let response = app
        .oneshot(
            Request::post("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(multipart))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(body_text(response).await.contains("Агульны памер файлаў"));
    assert!(db.list_uploads(10, 0).await.unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&config.temp_dir).unwrap().count(), 0);
}

async fn create_moderator(db: &Database) {
    db.create_user(
        "moderator",
        &hash_password(ADMIN_PASSWORD).unwrap(),
        now_ts(),
    )
    .await
    .unwrap();
}

async fn login(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::post("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "username=moderator&password={}",
                    ADMIN_PASSWORD.replace(' ', "+")
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    set_cookie.split(';').next().unwrap().to_string()
}

async fn category_request(
    app: &Router,
    cookie: &str,
    upload_id: &str,
    suffix: &str,
    csrf: &str,
    category: &str,
) -> Response {
    let body = form_body(&[("csrf_token", csrf), ("category", category)]);
    app.clone()
        .oneshot(
            Request::post(format!("/admin/uploads/{upload_id}{suffix}"))
                .header(header::COOKIE, cookie)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn publish_request(app: &Router, cookie: &str, csrf: &str, upload_id: &str) -> Response {
    let publish = form_body(&[("csrf_token", csrf), ("upload_id", upload_id)]);
    app.clone()
        .oneshot(
            Request::post("/admin/publish")
                .header(header::COOKIE, cookie)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(publish))
                .unwrap(),
        )
        .await
        .unwrap()
}

struct PendingPublishFixture {
    _root: tempfile::TempDir,
    db: Database,
    app: Router,
    upload: UploadRecord,
    stored_path: PathBuf,
    thumbnail_path: PathBuf,
    cookie: String,
    csrf: String,
}

impl PendingPublishFixture {
    async fn create(mock: &MockCommonsServer, filename: &str, color: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut config = support::test_config(root.path());
        config.commons_api_url = mock.api_url.clone();
        config.prepare_directories().unwrap();
        store_bot_password(&config.commons_bot_password_path, MOCK_BOT_PASSWORD).unwrap();
        let config = Arc::new(config);
        let db = Database::new(&config.database_path);
        db.initialize().await.unwrap();
        create_moderator(&db).await;
        let app = router(AppState::new(config.clone(), db.clone()).unwrap());

        let source = config.temp_dir.join("ambiguous-fixture.jpg");
        support::run(
            support::imagemagick()
                .args(["-size", "72x48", &format!("xc:{color}")])
                .arg(&source),
        );
        let jpeg = std::fs::read(source).unwrap();
        let boundary = "globustut-ambiguous-boundary";
        let response = app
            .clone()
            .oneshot(
                Request::post("/upload")
                    .header(
                        header::CONTENT_TYPE,
                        format!("multipart/form-data; boundary={boundary}"),
                    )
                    .body(Body::from(multipart_upload(
                        boundary,
                        filename,
                        &jpeg,
                        &["Category:Castles_in_Belarus"],
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let upload = db.list_uploads(10, 0).await.unwrap().pop().unwrap();
        let stored_path = config.upload_dir.join(&upload.stored_filename);
        let thumbnail_path = config.thumbnail_dir.join(format!("{}.webp", upload.id));
        let cookie = login(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::get("/admin")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let csrf = hidden_value(&body_text(response).await, "csrf_token");
        Self {
            _root: root,
            db,
            app,
            upload,
            stored_path,
            thumbnail_path,
            cookie,
            csrf,
        }
    }
}

fn mock_existing_file(upload: &UploadRecord, sha1: String, uploader: &str) -> MockExistingFile {
    let encoded = url::form_urlencoded::byte_serialize(upload.display_filename.as_bytes())
        .collect::<String>();
    MockExistingFile {
        filename: upload.display_filename.clone(),
        sha1,
        uploader: uploader.into(),
        comment: "Запампавана пасля прэмадэрацыі на globustut.by".into(),
        page_url: format!(
            "https://commons.wikimedia.org/wiki/File:{}",
            upload.display_filename.replace(' ', "_")
        ),
        file_url: format!("https://upload.wikimedia.org/mock/{encoded}"),
    }
}

fn multipart_upload(boundary: &str, filename: &str, jpeg: &[u8], categories: &[&str]) -> Vec<u8> {
    multipart_upload_many(boundary, &[(filename, jpeg)], categories)
}

fn multipart_upload_many(boundary: &str, files: &[(&str, &[u8])], categories: &[&str]) -> Vec<u8> {
    let mut body = Vec::new();
    for (filename, bytes) in files {
        add_part_header(
            &mut body,
            boundary,
            "files",
            Some(filename),
            Some("image/jpeg"),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    for category in categories {
        add_text_part(&mut body, boundary, "category", category);
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn add_part_header(
    body: &mut Vec<u8>,
    boundary: &str,
    name: &str,
    filename: Option<&str>,
    content_type: Option<&str>,
) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"").as_bytes());
    if let Some(filename) = filename {
        body.extend_from_slice(format!("; filename=\"{filename}\"").as_bytes());
    }
    body.extend_from_slice(b"\r\n");
    if let Some(content_type) = content_type {
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    }
    body.extend_from_slice(b"\r\n");
}

fn add_text_part(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    add_part_header(body, boundary, name, None, None);
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn form_body(fields: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in fields {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

fn sha1_hex_for_test(bytes: &[u8]) -> String {
    Sha1::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hidden_value(html: &str, name: &str) -> String {
    let marker = format!("name='{name}' value='");
    let start = html.find(&marker).expect("hidden input missing") + marker.len();
    let end = html[start..].find('\'').unwrap() + start;
    html[start..end].to_string()
}

async fn body_bytes(response: Response) -> Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

async fn body_text(response: Response) -> String {
    String::from_utf8(body_bytes(response).await.to_vec()).unwrap()
}

async fn body_json(response: Response) -> Value {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

#[derive(Clone, Default)]
struct MockCommonsState {
    calls: Arc<Mutex<MockCommonsCalls>>,
    behavior: Arc<Mutex<MockCommonsBehavior>>,
}

#[derive(Clone, Default)]
struct MockCommonsBehavior {
    existing_file: Option<MockExistingFile>,
    ambiguous_final_file: Option<MockExistingFile>,
    ambiguous_visibility_misses: usize,
    remaining_visibility_misses: usize,
    omit_final_imageinfo: bool,
    fail_url_enrichment: bool,
}

#[derive(Clone)]
struct MockExistingFile {
    filename: String,
    sha1: String,
    uploader: String,
    comment: String,
    page_url: String,
    file_url: String,
}

#[derive(Default)]
struct MockCommonsCalls {
    login_attempts: usize,
    title_checks: Vec<String>,
    url_enrichments: usize,
    chunk_uploads: usize,
    final_uploads: Vec<MockFinalUpload>,
}

struct MockFinalUpload {
    filename: String,
    text: String,
    comment: String,
}

struct MockCommonsServer {
    api_url: String,
    state: MockCommonsState,
    task: tokio::task::JoinHandle<()>,
}

impl MockCommonsServer {
    async fn spawn() -> Self {
        let state = MockCommonsState::default();
        let app = Router::new()
            .route("/w/api.php", get(mock_commons_get).post(mock_commons_post))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            api_url: format!("http://{address}/w/api.php"),
            state,
            task,
        }
    }
}

impl Drop for MockCommonsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn mock_commons_get(
    State(state): State<MockCommonsState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    match (
        query.get("meta").map(String::as_str),
        query.get("type").map(String::as_str),
    ) {
        (Some("tokens"), Some("login")) => {
            let mut response =
                Json(json!({"query": {"tokens": {"logintoken": "mock-login-token"}}}))
                    .into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static("mock_login=issued; Path=/; HttpOnly; SameSite=Lax"),
            );
            return response;
        }
        (Some("tokens|userinfo"), Some("csrf")) => {
            if !has_cookie(&headers, MOCK_SESSION_COOKIE) {
                return mock_api_error("assertuserfailed", "mock session is missing");
            }
            return Json(json!({
                "query": {
                    "tokens": {"csrftoken": "mock-csrf-token"},
                    "userinfo": {"id": 1, "name": "Globustut"}
                }
            }))
            .into_response();
        }
        _ => {}
    }

    if let Some(title) = query.get("titles") {
        if !has_cookie(&headers, MOCK_SESSION_COOKIE) {
            return mock_api_error("assertuserfailed", "mock session is missing");
        }
        let filename = title.strip_prefix("File:").unwrap_or(title).to_string();
        let iiprop = query.get("iiprop").map(String::as_str).unwrap_or_default();
        if iiprop.split('|').any(|property| property == "sha1") {
            state.calls.lock().unwrap().title_checks.push(filename);
            let existing = {
                let mut behavior = state.behavior.lock().unwrap();
                if behavior.remaining_visibility_misses != 0 {
                    behavior.remaining_visibility_misses -= 1;
                    None
                } else {
                    behavior.existing_file.clone()
                }
            };
            if let Some(existing) = existing {
                return Json(json!({
                    "query": {
                        "pages": [{
                            "title": format!("File:{}", existing.filename),
                            "imageinfo": [{
                                "url": existing.file_url,
                                "descriptionurl": existing.page_url,
                                "user": existing.uploader,
                                "comment": existing.comment,
                                "sha1": existing.sha1
                            }]
                        }]
                    }
                }))
                .into_response();
            }
            return Json(json!({"query": {"pages": [{"title": title, "missing": true}]}}))
                .into_response();
        }
        if iiprop == "url" {
            state.calls.lock().unwrap().url_enrichments += 1;
            if state.behavior.lock().unwrap().fail_url_enrichment {
                return mock_api_error("mock-url-lookup-failed", "URL lookup failed");
            }
        }
        return Json(json!({"query": {"pages": [{"title": title, "missing": true}]}}))
            .into_response();
    }

    mock_api_error("mock-unsupported-query", "unsupported mock query")
}

async fn mock_commons_post(
    State(state): State<MockCommonsState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if content_type.starts_with("application/x-www-form-urlencoded") {
        let form = url::form_urlencoded::parse(&body)
            .into_owned()
            .collect::<HashMap<_, _>>();
        state.calls.lock().unwrap().login_attempts += 1;
        let valid = has_cookie(&headers, MOCK_LOGIN_COOKIE)
            && form.get("action").map(String::as_str) == Some("login")
            && form.get("lgname").map(String::as_str) == Some("Globustut@globustut-uploader")
            && form.get("lgpassword").map(String::as_str) == Some(MOCK_BOT_PASSWORD)
            && form.get("lgtoken").map(String::as_str) == Some("mock-login-token");
        if !valid {
            return Json(json!({
                "login": {"result": "Failed", "reason": "invalid mock credentials"}
            }))
            .into_response();
        }
        let mut response = Json(json!({"login": {"result": "Success", "lgusername": "Globustut"}}))
            .into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("mock_session=authenticated; Path=/; HttpOnly; SameSite=Lax"),
        );
        return response;
    }

    if !content_type.starts_with("multipart/form-data")
        || !has_cookie(&headers, MOCK_SESSION_COOKIE)
    {
        return mock_api_error(
            "assertuserfailed",
            "authenticated multipart request required",
        );
    }
    let field = |name| multipart_field(&body, name);
    let authenticated = field("action").as_deref() == Some("upload")
        && field("assert").as_deref() == Some("user")
        && field("assertuser").as_deref() == Some("Globustut")
        && field("token").as_deref() == Some("mock-csrf-token");
    if !authenticated {
        return mock_api_error("badtoken", "invalid mock upload authentication");
    }

    if field("stash").as_deref() == Some("1") {
        let file_size = field("filesize")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        let offset = field("offset")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        if file_size == 0 || offset != 0 || field("chunk").is_none() {
            return mock_api_error("mock-invalid-chunk", "invalid chunk upload");
        }
        state.calls.lock().unwrap().chunk_uploads += 1;
        return Json(json!({
            "upload": {
                "result": "Continue",
                "offset": file_size,
                "filekey": "mock-stashed-file"
            }
        }))
        .into_response();
    }

    let filename = field("filename").unwrap_or_default();
    let text = field("text").unwrap_or_default();
    let comment = field("comment").unwrap_or_default();
    if field("filekey").as_deref() != Some("mock-stashed-file")
        || filename.is_empty()
        || text.is_empty()
        || comment.is_empty()
    {
        return mock_api_error("mock-invalid-final-upload", "invalid final upload");
    }
    state
        .calls
        .lock()
        .unwrap()
        .final_uploads
        .push(MockFinalUpload {
            filename: filename.clone(),
            text,
            comment,
        });
    let ambiguous_response = {
        let mut behavior = state.behavior.lock().unwrap();
        if let Some(existing) = behavior.ambiguous_final_file.take() {
            behavior.existing_file = Some(existing);
            behavior.remaining_visibility_misses = behavior.ambiguous_visibility_misses;
            true
        } else {
            false
        }
    };
    if ambiguous_response {
        return (StatusCode::OK, "{invalid Commons response").into_response();
    }
    if state.behavior.lock().unwrap().omit_final_imageinfo {
        Json(json!({
            "upload": {
                "result": "Success",
                "filename": filename
            }
        }))
        .into_response()
    } else {
        Json(json!({
            "upload": {
                "result": "Success",
                "filename": filename,
                "imageinfo": {
                    "descriptionurl": format!(
                        "https://commons.wikimedia.org/wiki/File:{}",
                        filename.replace(' ', "_")
                    ),
                    "url": format!(
                        "https://upload.wikimedia.org/mock/{}",
                        url::form_urlencoded::byte_serialize(filename.as_bytes()).collect::<String>()
                    )
                }
            }
        }))
        .into_response()
    }
}

fn multipart_field(body: &[u8], name: &str) -> Option<String> {
    let body = String::from_utf8_lossy(body);
    let marker = format!("name=\"{name}\"");
    let header_start = body.find(&marker)?;
    let value_start = body[header_start..].find("\r\n\r\n")? + header_start + 4;
    let value_end = body[value_start..].find("\r\n--")? + value_start;
    Some(body[value_start..value_end].to_string())
}

fn has_cookie(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .map(|cookies| cookies.split(';').any(|cookie| cookie.trim() == expected))
        .unwrap_or(false)
}

fn mock_api_error(code: &str, info: &str) -> Response {
    Json(json!({"error": {"code": code, "info": info}})).into_response()
}
