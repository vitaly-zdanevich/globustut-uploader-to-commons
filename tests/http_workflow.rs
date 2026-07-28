mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use globustut_uploader_to_commons::auth::{hash_password, now_ts};
use globustut_uploader_to_commons::db::Database;
use globustut_uploader_to_commons::media::FIXED_UPLOAD_LICENSE;
use globustut_uploader_to_commons::web::{AppState, router};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn upload_login_rename_category_and_delete_workflow() {
    let root = tempfile::tempdir().unwrap();
    let config = Arc::new(support::test_config(root.path()));
    config.prepare_directories().unwrap();
    let db = Database::new(&config.database_path);
    db.initialize().await.unwrap();
    db.create_user(
        "moderator",
        &hash_password("correct horse battery staple").unwrap(),
        now_ts(),
    )
    .await
    .unwrap();
    let app = router(AppState::new(config.clone(), db.clone()).unwrap());

    let source = config.temp_dir.join("fixture.jpg");
    support::run(
        support::imagemagick()
            .args(["-size", "80x60", "xc:#4cb6ac"])
            .arg(&source),
    );
    let jpeg = std::fs::read(source).unwrap();
    let boundary = "globustut-test-boundary";
    let multipart = multipart_upload(boundary, &jpeg);
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
    assert!(html.contains("Принято файлов: 1"));

    let upload = db.list_uploads(10, 0).await.unwrap().pop().unwrap();
    assert_eq!(upload.commons_category, "Castles in Belarus");
    assert_eq!(upload.display_filename, "castle.jpg");
    assert_eq!(upload.license, FIXED_UPLOAD_LICENSE);

    let response = app
        .clone()
        .oneshot(Request::get("/admin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::LOCATION], "/admin/login");

    let response = app
        .clone()
        .oneshot(
            Request::post("/admin/login")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(
                    "username=moderator&password=correct+horse+battery+staple",
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap().to_string();

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

    let category = form_body(&[
        ("csrf_token", csrf.as_str()),
        ("category", "Category:Castles_of_Belarus"),
    ]);
    let response = app
        .clone()
        .oneshot(
            Request::post(format!("/admin/uploads/{}/category", upload.id))
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(category))
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
            .commons_category,
        "Castles of Belarus"
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
    assert!(body_text(response).await.contains("Общий размер файлов"));
    assert!(db.list_uploads(10, 0).await.unwrap().is_empty());
    assert_eq!(std::fs::read_dir(&config.temp_dir).unwrap().count(), 0);
}

fn multipart_upload(boundary: &str, jpeg: &[u8]) -> Vec<u8> {
    multipart_upload_many(boundary, &[("castle.jpg", jpeg)])
}

fn multipart_upload_many(boundary: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
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
    add_text_part(
        &mut body,
        boundary,
        "category",
        "Category:Castles_in_Belarus",
    );
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

fn hidden_value(html: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = html.find(&marker).expect("hidden input missing") + marker.len();
    let end = html[start..].find('"').unwrap() + start;
    html[start..end].to_string()
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}
