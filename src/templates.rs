use crate::db::UploadRecord;
use crate::media::display_basename;
use html_escape::{encode_quoted_attribute, encode_text};

#[derive(Clone, Debug)]
pub struct SubmissionResult {
    pub filename: String,
    pub accepted: bool,
    pub detail: String,
}

pub fn upload_page(
    initial_category: &str,
    max_files: usize,
    max_file_mb: u64,
    max_request_mb: usize,
) -> String {
    let category = attribute(initial_category);
    let max_file_bytes = max_file_mb.saturating_mul(1024 * 1024);
    let max_request_bytes = max_request_mb.saturating_mul(1024 * 1024);
    let whole_gibibytes = max_request_mb / 1024;
    let max_request = if whole_gibibytes * 1024 == max_request_mb {
        format!("{whole_gibibytes} ГБ")
    } else {
        format!("{max_request_mb} МБ")
    };
    let body = format!(
        r#"
        <main class='upload-shell'>
          <header class='brand-header'>
            <a class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'><img src='/static/favicon.svg' width='44' height='44' alt=''></a>
            <div><strong>Глобус Беларусі</strong><span>Фатаграфіі для Wikimedia Commons</span></div>
          </header>
          <section class='upload-workspace' aria-labelledby='upload-title'>
            <div class='section-heading'>
              <h1 id='upload-title'>Дадаць фатаграфіі/відэа</h1>
            </div>
            <form class='upload-form' action='/upload' method='post' enctype='multipart/form-data' data-upload-form>
              <label class='file-drop' data-file-drop>
                <input type='file' name='files' multiple required data-file-input data-max-files='{max_files}' data-max-file-bytes='{max_file_bytes}' data-max-request-bytes='{max_request_bytes}'>
                <span class='file-drop-title'>Выбраць файлы</span>
                <span class='file-drop-meta'>Да {max_files} файлаў, кожны да {max_file_mb} МБ, агульны аб'ём да {max_request}</span>
              </label>
              <div class='selected-files' data-selected-files aria-live='polite'></div>
              <p class='file-error' role='alert' data-file-error hidden></p>
              <div class='field category-field'>
                <label for='upload-category'>Катэгорыя Wikimedia Commons</label>
                <input id='upload-category' type='text' name='category' value='{category}' maxlength='255' required autocomplete='off' spellcheck='false' role='combobox' aria-autocomplete='list' aria-haspopup='listbox' aria-controls='upload-category-menu' aria-expanded='false' data-category-input>
                <div id='upload-category-menu' class='category-menu' role='listbox' data-category-menu hidden></div>
              </div>
              <p class='license-note'>Запампоўваючы сюды свае фатаздымкі, вы публікуеце іх на сайтах Глобус Беларусі і Wikimedia Commons (Wikipedia) на ўмовах ліцэнзіі <strong>Creative Commons BY 4.0</strong>. Аўтар будзе пазначаны паводле метаданых.</p>
              <div class='form-actions'>
                <button class='primary-button' type='submit' data-submit>Адправіць на прэмадэрацыю</button>
                <span class='submit-status' data-submit-status aria-live='polite'></span>
              </div>
            </form>
          </section>
        </main>
        <footer class='upload-footer'>Дзякуй за захаванне культурнай спадчыны</footer>
        "#
    );
    page("Дадаць фатаграфіі/відэа", &body, "upload-page")
}

pub fn upload_results(results: &[SubmissionResult]) -> String {
    let accepted = results.iter().filter(|result| result.accepted).count();
    let mut rows = String::new();
    for result in results {
        let status = if result.accepted {
            "Прынята"
        } else {
            "Памылка"
        };
        let class = if result.accepted {
            "accepted"
        } else {
            "rejected"
        };
        rows.push_str(&format!(
            r#"<li class='result-row {class}'><span class='result-status'>{status}</span><strong>{}</strong><span>{}</span></li>"#,
            text(&result.filename),
            text(&result.detail)
        ));
    }
    let heading = if accepted == 0 {
        "Файлы не прыняты".to_string()
    } else {
        format!("Прынята файлаў: {accepted}")
    };
    let body = format!(
        r#"
        <main class='upload-shell result-shell'>
          <header class='brand-header'>
            <a class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'><img src='/static/favicon.svg' width='44' height='44' alt=''></a>
            <div><strong>Глобус Беларусі</strong><span>Фатаграфіі для Wikimedia Commons</span></div>
          </header>
          <section class='upload-workspace' aria-labelledby='result-title'>
            <div class='section-heading'><p class='eyebrow'>Прэмадэрацыя</p><h1 id='result-title'>{}</h1></div>
            <ul class='result-list'>{rows}</ul>
            <a class='primary-button button-link' href='/'>Дадаць яшчэ</a>
          </section>
        </main>
        "#,
        text(&heading)
    );
    page("Вынік запампоўвання", &body, "upload-page")
}

pub fn login_page(error: Option<&str>) -> String {
    let error = error
        .map(|message| {
            format!(
                r#"<p class='form-error' role='alert'>{}</p>"#,
                text(message)
            )
        })
        .unwrap_or_default();
    let body = format!(
        r#"
        <main class='login-shell'>
          <section class='login-panel' aria-labelledby='login-title'>
            <header class='brand-header compact'>
              <a class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'><img src='/static/favicon.svg' width='40' height='40' alt=''></a>
              <div><strong>Глобус Беларусі</strong><span>Прэмадэрацыя</span></div>
            </header>
            <h1 id='login-title'>Уваход</h1>
            {error}
            <form action='/admin/login' method='post' class='login-form'>
              <label class='field'><span>Імя карыстальніка</span><input name='username' autocomplete='username' maxlength='100' required autofocus></label>
              <label class='field'><span>Пароль</span><input type='password' name='password' autocomplete='current-password' required></label>
              <button class='primary-button' type='submit'>Увайсці</button>
            </form>
          </section>
        </main>
        "#
    );
    page("Уваход у прэмадэрацыю", &body, "admin-page")
}

pub struct AdminPageData<'a> {
    pub username: &'a str,
    pub csrf_token: &'a str,
    pub commons_account: Option<&'a str>,
    pub uploads: &'a [UploadRecord],
    pub total_count: u64,
    pub stored_bytes: u64,
    pub storage_limit_bytes: u64,
    pub page_number: usize,
    pub has_next_page: bool,
}

pub fn admin_page(data: AdminPageData<'_>) -> String {
    let mut items = String::new();
    for upload in data.uploads {
        items.push_str(&moderation_item(upload, data.csrf_token));
    }
    if data.uploads.is_empty() {
        items.push_str(
            r#"<div class='empty-state'><h2>Чарга пустая</h2><p>Новыя матэрыялы з'явяцца тут пасля адпраўкі формы.</p></div>"#,
        );
    }
    let disk_used_mib = mebibytes(data.stored_bytes);
    let storage_limit = format_storage_limit(data.storage_limit_bytes);
    let commons_files_link = data
        .commons_account
        .map(str::trim)
        .filter(|account| !account.is_empty())
        .map(|account| {
            let account = wiki_path_component(account);
            format!(
                r#"<a class='admin-files-link' href='https://commons.wikimedia.org/wiki/Special:ListFiles/{account}' target='_blank' rel='noopener noreferrer'>Запампаваныя файлы</a>"#
            )
        })
        .unwrap_or_default();
    let mut pending_upload_fields = String::new();
    for (index, upload) in data.uploads.iter().enumerate() {
        let name = if index == 0 { " name='upload_id'" } else { "" };
        pending_upload_fields.push_str(&format!(
            r#"<input type='hidden'{name} value='{}' data-publish-upload-id data-publish-size-bytes='{}'>"#,
            attribute(&upload.id),
            upload.size_bytes,
        ));
    }
    let publish_disabled = if data.uploads.is_empty() {
        " disabled aria-disabled='true'"
    } else {
        ""
    };
    let previous = (data.page_number > 1).then(|| {
        format!(
            r#"<a class='page-button' href='/admin?page={}'>Назад</a>"#,
            data.page_number - 1
        )
    });
    let next = data.has_next_page.then(|| {
        format!(
            r#"<a class='page-button' href='/admin?page={}'>Далей</a>"#,
            data.page_number + 1
        )
    });
    let pagination = if previous.is_some() || next.is_some() {
        format!(
            r#"<nav class='pagination' aria-label='Старонкі'>{}<span>Старонка {}</span>{}</nav>"#,
            previous.unwrap_or_default(),
            data.page_number,
            next.unwrap_or_default()
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"
        <header class='admin-header'>
          <div class='admin-header-primary'>
            <div class='admin-brand'>
              <a class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'><img src='/static/favicon.svg' width='34' height='34' alt=''></a>
              <a class='admin-brand-home' href='/admin'>Глобус Беларусі</a>
            </div>
            {commons_files_link}
          </div>
          <div class='admin-account'><span>{username}</span><form action='/admin/logout' method='post'><input type='hidden' name='csrf_token' value='{csrf}'><button class='quiet-button' type='submit'>Выйсці</button></form></div>
        </header>
        <main class='admin-main'>
          <div class='queue-heading'>
            <dl class='queue-stats' data-queue-stats data-total-count='{total_count}' data-stored-bytes='{stored_bytes}' data-storage-limit-bytes='{storage_limit_bytes}' data-storage-limit-label='{storage_limit}'>
              <div><dt>У чарзе</dt><dd data-queue-count>{total_count}</dd></div>
              <div><dt>Файлы</dt><dd data-queue-files>{stored_size}</dd></div>
              <div><dt>Дыск</dt><dd data-queue-disk>{disk_used_mib} МБ / {storage_limit}</dd></div>
            </dl>
          </div>
          <div class='moderation-list'>{items}</div>
          <div data-pagination-slot>{pagination}</div>
          <section class='commons-publish' aria-label='Публікацыя ў Wikimedia Commons'>
            <form class='commons-publish-form' action='/admin/publish' method='post' data-publish-form>
              <input type='hidden' name='csrf_token' value='{csrf}'>
              {pending_upload_fields}
              <button class='primary-button commons-publish-button' type='submit' data-publish-submit{publish_disabled}>
                <span>Запампаваць у Wikimedia Commons</span>
                <span class='publish-spinner' data-publish-spinner aria-hidden='true' hidden></span>
              </button>
              <p class='publish-status' role='status' aria-live='polite' data-publish-status></p>
            </form>
            <div class='publish-results' data-publish-results></div>
          </section>
        </main>
        "#,
        username = text(data.username),
        csrf = attribute(data.csrf_token),
        commons_files_link = commons_files_link,
        pending_upload_fields = pending_upload_fields,
        publish_disabled = publish_disabled,
        total_count = data.total_count,
        stored_bytes = data.stored_bytes,
        storage_limit_bytes = data.storage_limit_bytes,
        stored_size = format_size(data.stored_bytes),
    );
    page("Прэмадэрацыя", &body, "admin-page")
}

pub fn error_page(status: u16, message: &str) -> String {
    let body = format!(
        r#"<main class='error-shell'><span class='error-code'>{status}</span><h1>Не ўдалося выканаць запыт</h1><p>{}</p><a href='/' class='primary-button button-link'>На галоўную</a></main>"#,
        text(message)
    );
    page("Памылка", &body, "error-page")
}

fn moderation_item(upload: &UploadRecord, csrf_token: &str) -> String {
    let id = attribute(&upload.id);
    let media = if upload.media_kind == "video" {
        format!(r#"<video controls preload='metadata' src='/admin/media/{id}'></video>"#)
    } else {
        format!(
            r#"<a href='/admin/media/{id}' target='_blank' rel='noopener'><img src='/admin/thumbnail/{id}' alt='{}' loading='lazy'></a>"#,
            attribute(&upload.display_filename)
        )
    };
    let location_meta = match (upload.latitude, upload.longitude) {
        (Some(latitude), Some(longitude)) => {
            let osm = format!(
                "https://www.openstreetmap.org/?mlat={latitude:.6}&mlon={longitude:.6}#map=15/{latitude:.6}/{longitude:.6}"
            );
            format!(
                r#"<div><dt>Месца</dt><dd><a class='location-link' href='{}' target='_blank' rel='noopener noreferrer' aria-label='Адкрыць каардынаты {latitude:.6}, {longitude:.6} у OpenStreetMap'>{latitude:.6}, {longitude:.6}</a></dd></div>"#,
                attribute(&osm),
            )
        }
        _ => String::new(),
    };
    let mut category_chips = String::new();
    for category in &upload.commons_categories {
        let category_text = text(category);
        let category_value = attribute(category);
        let category_url = attribute(&commons_category_url(category));
        let open_label = attribute(&format!("Адкрыць катэгорыю {category} у Wikimedia Commons"));
        let remove_label = attribute(&format!("Выдаліць катэгорыю {category}"));
        category_chips.push_str(&format!(
            r#"
            <li class='category-chip' data-category-chip data-category-value='{category_value}'>
              <span class='category-chip-label'>{category_text}</span>
              <div class='category-chip-actions'>
                <a class='category-chip-link' href='{category_url}' target='_blank' rel='noopener noreferrer' title='{open_label}' aria-label='{open_label}'><span aria-hidden='true'>🔗</span></a>
                <form class='category-chip-remove-form' action='/admin/uploads/{id}/categories/remove' method='post' data-category-remove-form>
                  <input type='hidden' name='csrf_token' value='{csrf}'>
                  <input type='hidden' name='category' value='{category_value}'>
                  <button class='category-chip-remove' type='submit' title='{remove_label}' aria-label='{remove_label}' data-category-remove>×</button>
                </form>
              </div>
            </li>
            "#,
            csrf = attribute(csrf_token),
        ));
    }
    let extension = upload
        .display_filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    let conversion = if upload.converted {
        format!(
            "Пераўтворана · {} → {}",
            format_size(upload.original_size_bytes),
            format_size(upload.size_bytes)
        )
    } else {
        format_size(upload.size_bytes)
    };
    let dimensions = match (upload.width, upload.height) {
        (Some(width), Some(height)) => format!("{width}×{height}"),
        _ => "—".into(),
    };
    let author_meta = upload
        .author
        .as_deref()
        .map(|author| format!(r#"<div><dt>Аўтар</dt><dd>{}</dd></div>"#, text(author)))
        .unwrap_or_default();
    let uploaded_at = format_timestamp(upload.created_at);
    let (nearby_attribute, nearby_initial) = if upload.has_coordinates() {
        (
            format!(r#" data-nearby-url='/api/uploads/{id}/nearby-categories'"#),
            r#"<span class='muted'>Найбліжэйшыя катэгорыі запампоўваюцца…</span>"#,
        )
    } else {
        (
            String::new(),
            r#"<span class='muted'>Для пошуку найбліжэйшых катэгорый патрэбныя каардынаты EXIF.</span>"#,
        )
    };
    let category_input_id = format!("category-{id}");
    let category_menu_id = format!("category-menu-{id}");
    let category_label_id = format!("category-label-{id}");
    let category_status_id = format!("category-status-{id}");
    format!(
        r#"
        <article class='moderation-item' id='upload-{id}' data-upload-id='{id}'>
          <div class='media-stage'>{media}</div>
          <div class='moderation-fields'>
            <div class='item-topline'><span>{uploaded_at}</span></div>
            <form class='inline-form filename-form' action='/admin/uploads/{id}/rename' method='post'>
              <input type='hidden' name='csrf_token' value='{csrf}'>
              <label aria-label='Імя файла'><span class='filename-input'><input name='basename' value='{basename}' maxlength='180' required aria-label='Імя файла без пашырэння'><span>.{extension}</span></span></label>
              <button class='secondary-button' type='submit'>Захаваць імя</button>
            </form>
            <section class='category-editor' aria-labelledby='{category_label_id}' data-category-editor>
              <h2 class='category-editor-label' id='{category_label_id}'>Катэгорыі Commons</h2>
              <ul class='category-chip-list' data-category-list>{category_chips}</ul>
              <form class='category-add-form' action='/admin/uploads/{id}/categories' method='post' data-category-add-form>
                <input type='hidden' name='csrf_token' value='{csrf}'>
                <div class='field category-field'>
                  <label class='visually-hidden' for='{category_input_id}'>Дадаць катэгорыю Wikimedia Commons</label>
                  <input id='{category_input_id}' name='category' value='' maxlength='255' autocomplete='off' spellcheck='false' placeholder='Дадаць катэгорыю' role='combobox' aria-autocomplete='list' aria-haspopup='listbox' aria-controls='{category_menu_id}' aria-describedby='{category_status_id}' aria-expanded='false' data-category-input>
                  <div id='{category_menu_id}' class='category-menu' role='listbox' data-category-menu hidden></div>
                </div>
              </form>
              <p class='category-status' id='{category_status_id}' role='status' aria-live='polite' data-category-status></p>
            </section>
            <div class='category-context'><span class='category-context-label'>Найбліжэйшыя катэгорыі</span><div class='nearby-categories'{nearby_attribute}>{nearby_initial}</div></div>
            <dl class='file-meta'>
              <div><dt>Памер</dt><dd>{dimensions} · {conversion}</dd></div>
              {author_meta}
              {location_meta}
              <div><dt>Зыходнае імя</dt><dd title='{original_title}'>{original}</dd></div>
            </dl>
            <form class='delete-form' action='/admin/uploads/{id}/delete' method='post' data-delete-form>
              <input type='hidden' name='csrf_token' value='{csrf}'>
              <button class='delete-button' type='submit' title='Выдаліць файл' aria-label='Выдаліць файл'>×</button>
            </form>
          </div>
        </article>
        "#,
        csrf = attribute(csrf_token),
        basename = attribute(display_basename(&upload.display_filename)),
        extension = text(extension),
        category_chips = category_chips,
        author_meta = author_meta,
        location_meta = location_meta,
        original = text(&truncate(&upload.original_filename, 70)),
        original_title = attribute(&upload.original_filename),
    )
}

fn page(title: &str, body: &str, body_class: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang='be'>
<head>
  <meta charset='utf-8'>
  <meta name='viewport' content='width=device-width,initial-scale=1'>
  <meta name='color-scheme' content='light dark'>
  <title>{title}</title>
  <link rel='icon' href='/static/favicon.svg' type='image/svg+xml'>
  <link rel='stylesheet' href='/static/app.css'>
  <script src='/static/app.js' defer></script>
</head>
<body class='{body_class}'>{body}</body>
</html>"#,
        title = text(title),
        body_class = attribute(body_class)
    )
}

fn text(value: &str) -> String {
    encode_text(value).into_owned()
}

fn attribute(value: &str) -> String {
    encode_quoted_attribute(value).into_owned()
}

fn wiki_path_component(value: &str) -> String {
    let mut url = url::Url::parse("https://example.invalid/").expect("fixed URL must be valid");
    url.path_segments_mut()
        .expect("fixed URL must support path segments")
        .push(value);
    url.path().trim_start_matches('/').to_string()
}

fn commons_category_url(category: &str) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/Category:{}",
        wiki_path_component(&category.replace(' ', "_"))
    )
}

fn mebibytes(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

fn format_storage_limit(bytes: u64) -> String {
    let mebibytes = mebibytes(bytes);
    if mebibytes < 1000 {
        return format!("{mebibytes} МБ");
    }

    let whole_gigabytes = mebibytes / 1000;
    let tenths = (mebibytes % 1000) / 100;
    if tenths == 0 {
        format!("{whole_gigabytes} ГБ")
    } else {
        format!("{whole_gigabytes}.{tenths} ГБ")
    }
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn format_timestamp(timestamp: i64) -> String {
    let Ok(value) = time::OffsetDateTime::from_unix_timestamp(timestamp) else {
        return "—".into();
    };
    value
        .format(time::macros::format_description!(
            "[day].[month].[year] [hour]:[minute] UTC"
        ))
        .unwrap_or_else(|_| "—".into())
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::{
        AdminPageData, SubmissionResult, admin_page, login_page, upload_page, upload_results,
    };
    use crate::db::UploadRecord;

    #[test]
    fn escapes_dynamic_template_content() {
        let html = upload_results(&[SubmissionResult {
            filename: "<script>.jpg".into(),
            accepted: false,
            detail: "bad <input>".into(),
        }]);
        assert!(!html.contains("<script>.jpg"));
        assert!(html.contains("&lt;script&gt;.jpg"));

        let login = login_page(Some("bad <b>password</b>"));
        assert!(!login.contains("bad <b>password</b>"));
    }

    #[test]
    fn renders_public_upload_contract() {
        let html = upload_page("Castles_in_Belarus", 500, 200, 2048);

        assert!(html.contains("value='Castles_in_Belarus'"));
        assert!(html.contains("Да 500 файлаў, кожны да 200 МБ, агульны аб'ём да 2 ГБ"));
        assert!(html.contains("Creative Commons BY 4.0"));
        assert!(html.contains("Аўтар будзе пазначаны паводле метаданых."));
        assert!(html.contains("Дадаць фатаграфіі/відэа"));
        assert!(html.contains("Дзякуй за захаванне культурнай спадчыны"));
        assert!(html.contains(
            "class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'"
        ));
        assert!(!html.contains("Новая публикация"));
        assert!(!html.contains("class=\""));
    }

    #[test]
    fn renders_moderation_categories_and_publish_controls() {
        let upload = UploadRecord {
            id: "upload-one".into(),
            stored_filename: "stored.jpg".into(),
            original_filename: "original.jpg".into(),
            display_filename: "Photo.jpg".into(),
            mime_type: "image/jpeg".into(),
            media_kind: "image".into(),
            size_bytes: 1024,
            original_size_bytes: 1024,
            converted: false,
            width: Some(640),
            height: Some(480),
            latitude: Some(53.9045),
            longitude: Some(27.5615),
            capture_date: Some("2025-07-18".into()),
            author: None,
            commons_categories: vec!["Castles in Belarus".into(), "Minsk".into()],
            license: "cc-by-4.0".into(),
            uploader_username: "anonymous".into(),
            created_at: 1,
        };
        let mebibyte = 1024 * 1024;
        let html = admin_page(AdminPageData {
            username: "moderator",
            csrf_token: "csrf",
            commons_account: Some("Test Photographer"),
            uploads: &[upload],
            total_count: 1,
            stored_bytes: 100 * mebibyte,
            storage_limit_bytes: 2500 * mebibyte,
            page_number: 1,
            has_next_page: false,
        });

        assert_eq!(
            html.matches("data-category-chip data-category-value")
                .count(),
            2
        );
        assert!(html.contains("Category:Castles_in_Belarus"));
        assert!(html.contains("data-category-remove-form"));
        assert!(html.contains("data-category-add-form"));
        assert!(!html.contains("Сохранить категорию"));
        assert!(!html.contains("Открыть текущую категорию"));
        assert!(html.contains("Найбліжэйшыя катэгорыі"));
        assert!(html.contains(">53.904500, 27.561500</a>"));
        assert!(!html.contains("<dt>Аўтар</dt>"));
        assert!(!html.contains("<h1 class='queue-title'>"));
        assert!(!html.contains("<span class='visually-hidden'>Імя файла</span>"));
        assert!(!html.contains("Съёмка"));
        assert!(!html.contains("Лицензия"));
        assert!(html.contains("100 МБ / 2.5 ГБ"));
        assert!(html.contains("data-storage-limit-label='2.5 ГБ'"));
        assert!(html.contains(
            "class='brand-logo-link' href='https://globustut.by' aria-label='Глобус Беларусі'"
        ));
        assert!(html.contains("class='admin-brand-home' href='/admin'"));
        assert!(html.contains("Special:ListFiles/Test%20Photographer"));
        assert!(html.contains("name='upload_id' value='upload-one'"));
        assert!(html.contains("data-publish-size-bytes='1024'"));
        assert!(html.contains("data-publish-spinner"));
        assert!(html.contains("data-publish-results"));
        assert!(html.contains("Запампаваць у Wikimedia Commons"));
        assert!(!html.contains("Новые материалы"));
        assert!(!html.contains("class=\""));

        let empty_html = admin_page(AdminPageData {
            username: "moderator",
            csrf_token: "csrf",
            commons_account: None,
            uploads: &[],
            total_count: 0,
            stored_bytes: 0,
            storage_limit_bytes: 2500 * mebibyte,
            page_number: 1,
            has_next_page: false,
        });
        assert!(!empty_html.contains("admin-files-link"));
        assert!(empty_html.contains("data-publish-submit disabled"));
    }
}
