use crate::db::UploadRecord;
use crate::media::{DiskSpace, display_basename};
use html_escape::{encode_double_quoted_attribute, encode_text};

#[derive(Clone, Debug)]
pub struct SubmissionResult {
    pub filename: String,
    pub accepted: bool,
    pub detail: String,
}

pub fn upload_page(initial_category: &str, max_files: usize, max_file_mb: u64) -> String {
    let category = attribute(initial_category);
    let body = format!(
        r#"
        <main class="upload-shell">
          <header class="brand-header">
            <img src="/static/favicon.svg" width="44" height="44" alt="">
            <div><strong>Глобус Беларуси</strong><span>Фотографии для Wikimedia Commons</span></div>
          </header>
          <section class="upload-workspace" aria-labelledby="upload-title">
            <div class="section-heading">
              <p class="eyebrow">Новая публикация</p>
              <h1 id="upload-title">Добавить фотографии</h1>
            </div>
            <form class="upload-form" action="/upload" method="post" enctype="multipart/form-data" data-upload-form>
              <label class="file-drop" data-file-drop>
                <input type="file" name="files" multiple required data-file-input>
                <span class="file-drop-title">Выбрать файлы</span>
                <span class="file-drop-meta">До {max_files} файлов, каждый до {max_file_mb} МБ</span>
              </label>
              <div class="selected-files" data-selected-files aria-live="polite"></div>
              <div class="form-grid">
                <div class="field category-field">
                  <label for="upload-category">Категория Wikimedia Commons</label>
                  <input id="upload-category" type="text" name="category" value="{category}" maxlength="255" required autocomplete="off" spellcheck="false" role="combobox" aria-autocomplete="list" aria-haspopup="listbox" aria-controls="upload-category-menu" aria-expanded="false" data-category-input>
                  <div id="upload-category-menu" class="category-menu" role="listbox" data-category-menu hidden></div>
                </div>
                <label class="field">
                  <span>Свободная лицензия</span>
                  <select name="license" required>
                    <option value="cc-by-sa-4.0">CC BY-SA 4.0</option>
                    <option value="cc-by-4.0">CC BY 4.0</option>
                    <option value="cc0">CC0</option>
                  </select>
                </label>
              </div>
              <label class="rights-check">
                <input type="checkbox" name="rights_confirmed" value="yes" required>
                <span>У меня есть право опубликовать эти файлы под выбранной свободной лицензией.</span>
              </label>
              <div class="form-actions">
                <button class="primary-button" type="submit" data-submit>Отправить на премодерацию</button>
                <span class="submit-status" data-submit-status aria-live="polite"></span>
              </div>
            </form>
          </section>
        </main>
        "#
    );
    page("Добавить фотографии", &body, "upload-page")
}

pub fn upload_results(results: &[SubmissionResult]) -> String {
    let accepted = results.iter().filter(|result| result.accepted).count();
    let mut rows = String::new();
    for result in results {
        let status = if result.accepted {
            "Принято"
        } else {
            "Ошибка"
        };
        let class = if result.accepted {
            "accepted"
        } else {
            "rejected"
        };
        rows.push_str(&format!(
            r#"<li class="result-row {class}"><span class="result-status">{status}</span><strong>{}</strong><span>{}</span></li>"#,
            text(&result.filename),
            text(&result.detail)
        ));
    }
    let heading = if accepted == 0 {
        "Файлы не приняты".to_string()
    } else {
        format!("Принято файлов: {accepted}")
    };
    let body = format!(
        r#"
        <main class="upload-shell result-shell">
          <header class="brand-header">
            <img src="/static/favicon.svg" width="44" height="44" alt="">
            <div><strong>Глобус Беларуси</strong><span>Фотографии для Wikimedia Commons</span></div>
          </header>
          <section class="upload-workspace" aria-labelledby="result-title">
            <div class="section-heading"><p class="eyebrow">Премодерация</p><h1 id="result-title">{}</h1></div>
            <ul class="result-list">{rows}</ul>
            <a class="primary-button button-link" href="/">Добавить ещё</a>
          </section>
        </main>
        "#,
        text(&heading)
    );
    page("Результат загрузки", &body, "upload-page")
}

pub fn login_page(error: Option<&str>) -> String {
    let error = error
        .map(|message| {
            format!(
                r#"<p class="form-error" role="alert">{}</p>"#,
                text(message)
            )
        })
        .unwrap_or_default();
    let body = format!(
        r#"
        <main class="login-shell">
          <section class="login-panel" aria-labelledby="login-title">
            <header class="brand-header compact">
              <img src="/static/favicon.svg" width="40" height="40" alt="">
              <div><strong>Глобус Беларуси</strong><span>Премодерация</span></div>
            </header>
            <h1 id="login-title">Вход</h1>
            {error}
            <form action="/admin/login" method="post" class="login-form">
              <label class="field"><span>Логин</span><input name="username" autocomplete="username" maxlength="100" required autofocus></label>
              <label class="field"><span>Пароль</span><input type="password" name="password" autocomplete="current-password" required></label>
              <button class="primary-button" type="submit">Войти</button>
            </form>
          </section>
        </main>
        "#
    );
    page("Вход в премодерацию", &body, "admin-page")
}

pub struct AdminPageData<'a> {
    pub username: &'a str,
    pub csrf_token: &'a str,
    pub uploads: &'a [UploadRecord],
    pub total_count: u64,
    pub stored_bytes: u64,
    pub disk: &'a DiskSpace,
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
            r#"<div class="empty-state"><h2>Очередь пуста</h2><p>Новые фотографии появятся здесь после отправки формы.</p></div>"#,
        );
    }
    let used_percent = if data.disk.total_bytes == 0 {
        0.0
    } else {
        ((data
            .disk
            .total_bytes
            .saturating_sub(data.disk.available_bytes)) as f64
            / data.disk.total_bytes as f64)
            * 100.0
    };
    let previous = (data.page_number > 1).then(|| {
        format!(
            r#"<a class="page-button" href="/admin?page={}">Назад</a>"#,
            data.page_number - 1
        )
    });
    let next = data.has_next_page.then(|| {
        format!(
            r#"<a class="page-button" href="/admin?page={}">Далее</a>"#,
            data.page_number + 1
        )
    });
    let pagination = if previous.is_some() || next.is_some() {
        format!(
            r#"<nav class="pagination" aria-label="Страницы">{}<span>Страница {}</span>{}</nav>"#,
            previous.unwrap_or_default(),
            data.page_number,
            next.unwrap_or_default()
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"
        <header class="admin-header">
          <a class="admin-brand" href="/admin"><img src="/static/favicon.svg" width="34" height="34" alt=""><span>Глобус Беларуси</span></a>
          <div class="admin-account"><span>{username}</span><form action="/admin/logout" method="post"><input type="hidden" name="csrf_token" value="{csrf}"><button class="quiet-button" type="submit">Выйти</button></form></div>
        </header>
        <main class="admin-main">
          <div class="queue-heading">
            <div><p class="eyebrow">Премодерация</p><h1>Новые материалы</h1></div>
            <dl class="queue-stats">
              <div><dt>В очереди</dt><dd>{total_count}</dd></div>
              <div><dt>Файлы</dt><dd>{stored_size}</dd></div>
              <div><dt>Диск</dt><dd>{used_percent:.0}%</dd></div>
            </dl>
          </div>
          <div class="moderation-list">{items}</div>
          {pagination}
        </main>
        "#,
        username = text(data.username),
        csrf = attribute(data.csrf_token),
        total_count = data.total_count,
        stored_size = format_size(data.stored_bytes),
    );
    page("Премодерация", &body, "admin-page")
}

pub fn error_page(status: u16, message: &str) -> String {
    let body = format!(
        r#"<main class="error-shell"><span class="error-code">{status}</span><h1>Не удалось выполнить запрос</h1><p>{}</p><a href="/" class="primary-button button-link">На главную</a></main>"#,
        text(message)
    );
    page("Ошибка", &body, "error-page")
}

fn moderation_item(upload: &UploadRecord, csrf_token: &str) -> String {
    let id = attribute(&upload.id);
    let media = if upload.media_kind == "video" {
        format!(r#"<video controls preload="metadata" src="/admin/media/{id}"></video>"#)
    } else {
        format!(
            r#"<a href="/admin/media/{id}" target="_blank" rel="noopener"><img src="/admin/thumbnail/{id}" alt="{}" loading="lazy"></a>"#,
            attribute(&upload.display_filename)
        )
    };
    let coordinates = match (upload.latitude, upload.longitude) {
        (Some(latitude), Some(longitude)) => {
            let osm = format!(
                "https://www.openstreetmap.org/?mlat={latitude:.6}&mlon={longitude:.6}#map=15/{latitude:.6}/{longitude:.6}"
            );
            format!(
                r#"<a class="location-link" href="{}" target="_blank" rel="noopener noreferrer">OpenStreetMap · {latitude:.6}, {longitude:.6}</a>"#,
                attribute(&osm)
            )
        }
        _ => r#"<span class="muted">Координат EXIF нет</span>"#.into(),
    };
    let category_url = format!(
        "https://commons.wikimedia.org/wiki/Category:{}",
        url::form_urlencoded::byte_serialize(upload.commons_category.replace(' ', "_").as_bytes())
            .collect::<String>()
    );
    let extension = upload
        .display_filename
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("");
    let conversion = if upload.converted {
        format!(
            "Конвертирован · {} → {}",
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
    let date = upload
        .capture_date
        .as_deref()
        .unwrap_or("Дата EXIF не указана");
    let uploaded_at = format_timestamp(upload.created_at);
    let (nearby_attribute, nearby_initial) = if upload.has_coordinates() {
        (
            format!(r#" data-nearby-url="/api/uploads/{id}/nearby-categories""#),
            r#"<span class="muted">Ближайшие категории загружаются…</span>"#,
        )
    } else {
        (
            String::new(),
            r#"<span class="muted">Для поиска ближайших категорий нужны координаты EXIF.</span>"#,
        )
    };
    let category_input_id = format!("category-{id}");
    let category_menu_id = format!("category-menu-{id}");
    format!(
        r#"
        <article class="moderation-item" id="upload-{id}" data-upload-id="{id}">
          <div class="media-stage">{media}</div>
          <div class="moderation-fields">
            <div class="item-topline"><span>{kind}</span><span>{uploaded_at}</span></div>
            <form class="inline-form filename-form" action="/admin/uploads/{id}/rename" method="post">
              <input type="hidden" name="csrf_token" value="{csrf}">
              <label><span>Имя файла</span><span class="filename-input"><input name="basename" value="{basename}" maxlength="180" required><span>.{extension}</span></span></label>
              <button class="secondary-button" type="submit">Сохранить имя</button>
            </form>
            <form class="inline-form category-form" action="/admin/uploads/{id}/category" method="post">
              <input type="hidden" name="csrf_token" value="{csrf}">
              <div class="field category-field"><label for="{category_input_id}">Категория Commons</label><input id="{category_input_id}" name="category" value="{category}" maxlength="255" autocomplete="off" spellcheck="false" required role="combobox" aria-autocomplete="list" aria-haspopup="listbox" aria-controls="{category_menu_id}" aria-expanded="false" data-category-input><div id="{category_menu_id}" class="category-menu" role="listbox" data-category-menu hidden></div></div>
              <button class="secondary-button" type="submit">Сохранить категорию</button>
            </form>
            <div class="category-context"><a href="{category_url}" target="_blank" rel="noopener">Открыть текущую категорию</a><div class="nearby-categories"{nearby_attribute}>{nearby_initial}</div></div>
            <dl class="file-meta">
              <div><dt>Размер</dt><dd>{dimensions} · {conversion}</dd></div>
              <div><dt>Съёмка</dt><dd>{date}</dd></div>
              <div><dt>Место</dt><dd>{coordinates}</dd></div>
              <div><dt>Лицензия</dt><dd>{license}</dd></div>
              <div><dt>Исходное имя</dt><dd title="{original_title}">{original}</dd></div>
            </dl>
            <form class="delete-form" action="/admin/uploads/{id}/delete" method="post" data-delete-form>
              <input type="hidden" name="csrf_token" value="{csrf}">
              <button class="delete-button" type="submit" title="Удалить файл" aria-label="Удалить файл">×</button>
            </form>
          </div>
        </article>
        "#,
        kind = if upload.media_kind == "video" {
            "Видео"
        } else {
            "Фото"
        },
        csrf = attribute(csrf_token),
        basename = attribute(display_basename(&upload.display_filename)),
        extension = text(extension),
        category = attribute(&upload.commons_category),
        category_url = attribute(&category_url),
        date = text(date),
        coordinates = coordinates,
        license = text(license_label(&upload.license)),
        original = text(&truncate(&upload.original_filename, 70)),
        original_title = attribute(&upload.original_filename),
    )
}

fn page(title: &str, body: &str, body_class: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <meta name="color-scheme" content="light dark">
  <title>{title}</title>
  <link rel="icon" href="/static/favicon.svg" type="image/svg+xml">
  <link rel="stylesheet" href="/static/app.css">
  <script src="/static/app.js" defer></script>
</head>
<body class="{body_class}">{body}</body>
</html>"#,
        title = text(title),
        body_class = attribute(body_class)
    )
}

fn text(value: &str) -> String {
    encode_text(value).into_owned()
}

fn attribute(value: &str) -> String {
    encode_double_quoted_attribute(value).into_owned()
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

fn license_label(value: &str) -> &str {
    match value {
        "cc-by-sa-4.0" => "CC BY-SA 4.0",
        "cc-by-4.0" => "CC BY 4.0",
        "cc0" => "CC0",
        _ => value,
    }
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
    use super::{SubmissionResult, login_page, upload_results};

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
}
