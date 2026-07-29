use crate::commons::validate_categories;
use anyhow::{Context, Result, bail};
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Database {
    path: Arc<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub username: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct UploadRecord {
    pub id: String,
    pub stored_filename: String,
    pub original_filename: String,
    pub display_filename: String,
    pub mime_type: String,
    pub media_kind: String,
    pub size_bytes: u64,
    pub original_size_bytes: u64,
    pub converted: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub capture_date: Option<String>,
    pub author: Option<String>,
    pub commons_categories: Vec<String>,
    pub license: String,
    pub uploader_username: String,
    pub created_at: i64,
}

impl UploadRecord {
    pub fn has_coordinates(&self) -> bool {
        self.latitude.is_some() && self.longitude.is_some()
    }
}

impl Database {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Arc::new(path.into()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn initialize(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection.execute_batch(
                r#"
                PRAGMA foreign_keys = ON;
                PRAGMA journal_mode = DELETE;
                PRAGMA synchronous = FULL;

                CREATE TABLE IF NOT EXISTS users (
                    username TEXT PRIMARY KEY COLLATE NOCASE,
                    password_hash TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS sessions (
                    token_hash BLOB PRIMARY KEY,
                    username TEXT NOT NULL,
                    csrf_token TEXT NOT NULL,
                    expires_at INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS sessions_expires_at_idx
                    ON sessions(expires_at);

                CREATE TABLE IF NOT EXISTS uploads (
                    id TEXT PRIMARY KEY,
                    stored_filename TEXT NOT NULL UNIQUE,
                    original_filename TEXT NOT NULL,
                    display_filename TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    media_kind TEXT NOT NULL CHECK (media_kind IN ('image', 'video')),
                    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
                    original_size_bytes INTEGER NOT NULL CHECK (original_size_bytes >= 0),
                    converted INTEGER NOT NULL CHECK (converted IN (0, 1)),
                    width INTEGER,
                    height INTEGER,
                    latitude REAL,
                    longitude REAL,
                    capture_date TEXT,
                    author TEXT,
                    commons_category TEXT NOT NULL,
                    commons_categories_json TEXT,
                    license TEXT NOT NULL,
                    uploader_username TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS uploads_created_at_idx
                    ON uploads(created_at DESC);
                "#,
            )?;
            migrate_schema(connection)?;
            Ok(())
        })
        .await?;
        protect_database_file(self.path())?;
        Ok(())
    }

    pub async fn cleanup_expired_auth(&self, now: i64) -> Result<()> {
        self.with_connection(move |connection| {
            connection.execute("DELETE FROM sessions WHERE expires_at <= ?1", [now])?;
            Ok(())
        })
        .await
    }

    pub async fn create_user(&self, username: &str, password_hash: &str, now: i64) -> Result<()> {
        let username = normalized_username(username)?;
        let password_hash = password_hash.to_string();
        self.with_connection(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            transaction.execute(
                "INSERT INTO users(username, password_hash, created_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash",
                params![&username, password_hash, now],
            )?;
            transaction.execute(
                "DELETE FROM sessions WHERE username COLLATE NOCASE = ?1",
                [&username],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }

    pub async fn delete_user(&self, username: &str) -> Result<bool> {
        let username = normalized_username(username)?;
        self.with_connection(move |connection| {
            let transaction = connection.unchecked_transaction()?;
            let deleted =
                transaction.execute("DELETE FROM users WHERE username = ?1", [&username])?;
            transaction.execute(
                "DELETE FROM sessions WHERE username COLLATE NOCASE = ?1",
                [&username],
            )?;
            transaction.commit()?;
            Ok(deleted > 0)
        })
        .await
    }

    pub async fn list_users(&self) -> Result<Vec<String>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT username FROM users ORDER BY username COLLATE NOCASE ASC")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn password_hash(&self, username: &str) -> Result<Option<String>> {
        let username = username.to_string();
        self.with_connection(move |connection| {
            connection
                .query_row(
                    "SELECT password_hash FROM users WHERE username = ?1",
                    [username],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn insert_session(
        &self,
        token_hash: Vec<u8>,
        username: String,
        csrf_token: String,
        expires_at: i64,
        now: i64,
    ) -> Result<()> {
        self.with_connection(move |connection| {
            connection.execute(
                "INSERT INTO sessions(token_hash, username, csrf_token, expires_at, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![token_hash, username, csrf_token, expires_at, now],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn insert_session_if_password_hash(
        &self,
        token_hash: Vec<u8>,
        username: String,
        csrf_token: String,
        expires_at: i64,
        now: i64,
        expected_password_hash: String,
    ) -> Result<bool> {
        self.with_connection(move |connection| {
            Ok(connection.execute(
                "INSERT INTO sessions(token_hash, username, csrf_token, expires_at, created_at) \
                 SELECT ?1, ?2, ?3, ?4, ?5 \
                 WHERE EXISTS ( \
                    SELECT 1 FROM users \
                    WHERE username = ?2 AND password_hash = ?6 \
                 )",
                params![
                    token_hash,
                    username,
                    csrf_token,
                    expires_at,
                    now,
                    expected_password_hash
                ],
            )? > 0)
        })
        .await
    }

    pub async fn get_session(
        &self,
        token_hash: Vec<u8>,
        now: i64,
    ) -> Result<Option<SessionRecord>> {
        self.with_connection(move |connection| {
            connection
                .query_row(
                    "SELECT username, csrf_token, expires_at FROM sessions \
                     WHERE token_hash = ?1 AND expires_at > ?2",
                    params![token_hash, now],
                    |row| {
                        Ok(SessionRecord {
                            username: row.get(0)?,
                            csrf_token: row.get(1)?,
                            expires_at: row.get(2)?,
                        })
                    },
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn delete_session(&self, token_hash: Vec<u8>) -> Result<()> {
        self.with_connection(move |connection| {
            connection.execute("DELETE FROM sessions WHERE token_hash = ?1", [token_hash])?;
            Ok(())
        })
        .await
    }

    pub async fn insert_upload(&self, upload: UploadRecord) -> Result<()> {
        let commons_categories = validate_categories(&upload.commons_categories)?;
        let commons_category = commons_categories
            .first()
            .context("validated category list is empty")?
            .clone();
        let commons_categories_json =
            serde_json::to_string(&commons_categories).context("failed to encode categories")?;
        self.with_connection(move |connection| {
            connection.execute(
                r#"INSERT INTO uploads(
                    id, stored_filename, original_filename, display_filename,
                    mime_type, media_kind, size_bytes, original_size_bytes,
                    converted, width, height, latitude, longitude, capture_date,
                    author, commons_category, commons_categories_json, license,
                    uploader_username, created_at
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
                )"#,
                params![
                    upload.id,
                    upload.stored_filename,
                    upload.original_filename,
                    upload.display_filename,
                    upload.mime_type,
                    upload.media_kind,
                    upload.size_bytes,
                    upload.original_size_bytes,
                    upload.converted,
                    upload.width,
                    upload.height,
                    upload.latitude,
                    upload.longitude,
                    upload.capture_date,
                    upload.author,
                    commons_category,
                    commons_categories_json,
                    upload.license,
                    upload.uploader_username,
                    upload.created_at,
                ],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn list_uploads(&self, limit: usize, offset: usize) -> Result<Vec<UploadRecord>> {
        self.with_connection(move |connection| {
            let mut statement = connection.prepare(
                r#"SELECT
                    id, stored_filename, original_filename, display_filename,
                    mime_type, media_kind, size_bytes, original_size_bytes,
                    converted, width, height, latitude, longitude, capture_date,
                    author, commons_category, commons_categories_json, license,
                    uploader_username, created_at
                FROM uploads
                ORDER BY created_at DESC, id DESC
                LIMIT ?1 OFFSET ?2"#,
            )?;
            let rows = statement.query_map(params![limit, offset], row_to_upload)?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn get_upload(&self, id: &str) -> Result<Option<UploadRecord>> {
        let id = id.to_string();
        self.with_connection(move |connection| {
            connection
                .query_row(
                    r#"SELECT
                        id, stored_filename, original_filename, display_filename,
                        mime_type, media_kind, size_bytes, original_size_bytes,
                        converted, width, height, latitude, longitude, capture_date,
                        author, commons_category, commons_categories_json, license,
                        uploader_username, created_at
                    FROM uploads WHERE id = ?1"#,
                    [id],
                    row_to_upload,
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn update_display_filename(&self, id: &str, display_filename: &str) -> Result<bool> {
        let id = id.to_string();
        let display_filename = display_filename.to_string();
        self.with_connection(move |connection| {
            Ok(connection.execute(
                "UPDATE uploads SET display_filename = ?1 WHERE id = ?2",
                params![display_filename, id],
            )? > 0)
        })
        .await
    }

    pub async fn replace_categories<S: AsRef<str>>(
        &self,
        id: &str,
        categories: &[S],
    ) -> Result<bool> {
        let id = id.to_string();
        let categories = validate_categories(categories)?;
        let primary_category = categories
            .first()
            .context("validated category list is empty")?
            .clone();
        let categories_json =
            serde_json::to_string(&categories).context("failed to encode categories")?;
        self.with_connection(move |connection| {
            Ok(connection.execute(
                "UPDATE uploads \
                 SET commons_category = ?1, commons_categories_json = ?2 \
                 WHERE id = ?3",
                params![primary_category, categories_json, id],
            )? > 0)
        })
        .await
    }

    pub async fn delete_upload(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.with_connection(move |connection| {
            Ok(connection.execute("DELETE FROM uploads WHERE id = ?1", [id])? > 0)
        })
        .await
    }

    pub async fn upload_totals(&self) -> Result<(u64, u64)> {
        self.with_connection(|connection| {
            connection
                .query_row(
                    "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM uploads",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(Into::into)
        })
        .await
    }

    pub async fn storage_references(&self) -> Result<Vec<(String, String, String)>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare("SELECT id, stored_filename, media_kind FROM uploads ORDER BY id ASC")?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    async fn with_connection<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
    {
        let path = self.path.as_ref().clone();
        tokio::task::spawn_blocking(move || {
            let connection = Connection::open(&path)
                .with_context(|| format!("failed to open SQLite at {}", path.display()))?;
            connection.busy_timeout(Duration::from_secs(5))?;
            connection.execute_batch("PRAGMA foreign_keys = ON;")?;
            operation(&connection)
        })
        .await
        .context("SQLite worker stopped unexpectedly")?
    }
}

fn row_to_upload(row: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRecord> {
    let legacy_category = row.get::<_, String>(15)?;
    let categories_json = row.get::<_, Option<String>>(16)?;
    let commons_categories = decode_categories(&legacy_category, categories_json.as_deref(), 16)?;
    Ok(UploadRecord {
        id: row.get(0)?,
        stored_filename: row.get(1)?,
        original_filename: row.get(2)?,
        display_filename: row.get(3)?,
        mime_type: row.get(4)?,
        media_kind: row.get(5)?,
        size_bytes: row.get(6)?,
        original_size_bytes: row.get(7)?,
        converted: row.get(8)?,
        width: row.get(9)?,
        height: row.get(10)?,
        latitude: row.get(11)?,
        longitude: row.get(12)?,
        capture_date: row.get(13)?,
        author: row.get(14)?,
        commons_categories,
        license: row.get(17)?,
        uploader_username: row.get(18)?,
        created_at: row.get(19)?,
    })
}

fn decode_categories(
    legacy_category: &str,
    categories_json: Option<&str>,
    column_index: usize,
) -> rusqlite::Result<Vec<String>> {
    let Some(categories_json) = categories_json else {
        return Ok(vec![legacy_category.to_string()]);
    };
    let categories: Vec<String> = serde_json::from_str(categories_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(column_index, Type::Text, Box::new(error))
    })?;
    let validated = validate_categories(&categories)
        .map_err(|error| invalid_category_list(column_index, error.to_string()))?;
    if validated != categories {
        return Err(invalid_category_list(
            column_index,
            "stored category list is not normalized or contains duplicates",
        ));
    }
    if categories.first().map(String::as_str) != Some(legacy_category) {
        return Err(invalid_category_list(
            column_index,
            "legacy primary category does not match stored category list",
        ));
    }
    Ok(categories)
}

fn invalid_category_list(column_index: usize, message: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column_index,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message.into(),
        )),
    )
}

fn migrate_schema(connection: &Connection) -> Result<()> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))?;
    if version >= 3 {
        return Ok(());
    }

    let transaction = connection.unchecked_transaction()?;
    if version < 2 && !table_has_column(&transaction, "uploads", "author")? {
        transaction.execute("ALTER TABLE uploads ADD COLUMN author TEXT", [])?;
    }
    // Legacy rows keep using commons_category until a moderator replaces their list.
    if !table_has_column(&transaction, "uploads", "commons_categories_json")? {
        transaction.execute(
            "ALTER TABLE uploads ADD COLUMN commons_categories_json TEXT",
            [],
        )?;
    }
    transaction.pragma_update(None, "user_version", 3)?;
    transaction.commit()?;
    Ok(())
}

fn table_has_column(connection: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    Ok(columns
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|candidate| candidate == column))
}

fn normalized_username(username: &str) -> Result<String> {
    let username = username.trim();
    if username.is_empty() || username.len() > 100 || username.chars().any(char::is_control) {
        bail!("invalid username");
    }
    Ok(username.to_string())
}

#[cfg(unix)]
fn protect_database_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to protect {}", path.display()))
}

#[cfg(not(unix))]
fn protect_database_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Database, UploadRecord};

    fn upload(id: &str) -> UploadRecord {
        UploadRecord {
            id: id.into(),
            stored_filename: format!("{id}.jpg"),
            original_filename: "original.jpg".into(),
            display_filename: "Original.jpg".into(),
            mime_type: "image/jpeg".into(),
            media_kind: "image".into(),
            size_bytes: 10,
            original_size_bytes: 10,
            converted: false,
            width: Some(1),
            height: Some(1),
            latitude: Some(53.9),
            longitude: Some(27.5667),
            capture_date: None,
            author: Some("Example Photographer".into()),
            commons_categories: vec!["Minsk".into(), "Belarus".into()],
            license: "cc-by-4.0".into(),
            uploader_username: "Example".into(),
            created_at: 1,
        }
    }

    #[tokio::test]
    async fn persists_admins_sessions_and_uploads() {
        let directory = tempfile::tempdir().unwrap();
        let db = Database::new(directory.path().join("test.sqlite3"));
        db.initialize().await.unwrap();

        db.create_user("Example_User", "hash", 1).await.unwrap();
        assert_eq!(
            db.password_hash("Example_User").await.unwrap().as_deref(),
            Some("hash")
        );
        assert!(db.password_hash("Example User").await.unwrap().is_none());

        db.insert_session(vec![1], "Example_User".into(), "csrf".into(), 10, 1)
            .await
            .unwrap();
        assert_eq!(
            db.get_session(vec![1], 2).await.unwrap().unwrap().username,
            "Example_User"
        );

        db.create_user("example_user", "new-hash", 2).await.unwrap();
        assert!(db.get_session(vec![1], 2).await.unwrap().is_none());
        assert_eq!(
            db.password_hash("Example_User").await.unwrap().as_deref(),
            Some("new-hash")
        );
        assert!(
            !db.insert_session_if_password_hash(
                vec![2],
                "Example_User".into(),
                "csrf".into(),
                10,
                2,
                "hash".into(),
            )
            .await
            .unwrap()
        );
        assert!(
            db.insert_session_if_password_hash(
                vec![3],
                "Example_User".into(),
                "csrf".into(),
                10,
                2,
                "new-hash".into(),
            )
            .await
            .unwrap()
        );

        db.insert_upload(upload("one")).await.unwrap();
        let mut second_upload = upload("two");
        second_upload.created_at = 2;
        db.insert_upload(second_upload).await.unwrap();
        let mut tied_upload = upload("zulu");
        tied_upload.created_at = 2;
        db.insert_upload(tied_upload).await.unwrap();
        let uploads = db.list_uploads(10, 0).await.unwrap();
        assert_eq!(uploads.len(), 3);
        assert_eq!(uploads[2].author.as_deref(), Some("Example Photographer"));
        assert_eq!(uploads[2].commons_categories, ["Minsk", "Belarus"]);
        assert!(
            db.replace_categories(
                "one",
                &["Category:Castles_in_Belarus", "Minsk", "Castles in Belarus",],
            )
            .await
            .unwrap()
        );
        assert_eq!(
            db.get_upload("one")
                .await
                .unwrap()
                .unwrap()
                .commons_categories,
            ["Castles in Belarus", "Minsk"]
        );
        assert!(
            db.replace_categories::<&str>("one", &[]).await.is_err(),
            "an upload must retain at least one category"
        );
        assert!(
            db.replace_categories("one", &["Minsk", "Bad[title]"])
                .await
                .is_err()
        );
        assert_eq!(
            db.get_upload("one")
                .await
                .unwrap()
                .unwrap()
                .commons_categories,
            ["Castles in Belarus", "Minsk"],
            "a rejected replacement must not partially update the upload"
        );
        assert!(!db.replace_categories("missing", &["Minsk"]).await.unwrap());

        let connection = rusqlite::Connection::open(db.path()).unwrap();
        let stored: (String, String) = connection
            .query_row(
                "SELECT commons_category, commons_categories_json FROM uploads WHERE id = 'one'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, "Castles in Belarus");
        assert_eq!(stored.1, r#"["Castles in Belarus","Minsk"]"#);
        drop(connection);
    }

    #[tokio::test]
    async fn migrates_v1_uploads_and_preserves_existing_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE users (
                    username TEXT PRIMARY KEY COLLATE NOCASE,
                    password_hash TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO users VALUES ('admin', 'existing-hash', 1);

                CREATE TABLE uploads (
                    id TEXT PRIMARY KEY,
                    stored_filename TEXT NOT NULL UNIQUE,
                    original_filename TEXT NOT NULL,
                    display_filename TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    media_kind TEXT NOT NULL CHECK (media_kind IN ('image', 'video')),
                    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
                    original_size_bytes INTEGER NOT NULL CHECK (original_size_bytes >= 0),
                    converted INTEGER NOT NULL CHECK (converted IN (0, 1)),
                    width INTEGER,
                    height INTEGER,
                    latitude REAL,
                    longitude REAL,
                    capture_date TEXT,
                    commons_category TEXT NOT NULL,
                    license TEXT NOT NULL,
                    uploader_username TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO uploads VALUES (
                    'legacy', 'legacy.jpg', 'legacy.jpg', 'Legacy.jpg',
                    'image/jpeg', 'image', 10, 10, 0, 1, 1, NULL, NULL, NULL,
                    'Minsk', 'cc-by-4.0', 'anonymous', 1
                );
                PRAGMA user_version = 1;
                "#,
            )
            .unwrap();
        drop(connection);

        let db = Database::new(&path);
        db.initialize().await.unwrap();
        db.initialize().await.unwrap();
        assert_eq!(
            db.password_hash("admin").await.unwrap().as_deref(),
            Some("existing-hash")
        );

        let legacy = db.get_upload("legacy").await.unwrap().unwrap();
        assert!(legacy.author.is_none());
        assert_eq!(legacy.commons_categories, ["Minsk"]);
        db.insert_upload(upload("new")).await.unwrap();
        assert_eq!(
            db.get_upload("new")
                .await
                .unwrap()
                .unwrap()
                .author
                .as_deref(),
            Some("Example Photographer")
        );

        let connection = rusqlite::Connection::open(&path).unwrap();
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        let author_columns: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('uploads') WHERE name = 'author'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let category_columns: u32 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('uploads') \
                 WHERE name = 'commons_categories_json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let legacy_json: Option<String> = connection
            .query_row(
                "SELECT commons_categories_json FROM uploads WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 3);
        assert_eq!(author_columns, 1);
        assert_eq!(category_columns, 1);
        assert!(legacy_json.is_none());
    }

    #[tokio::test]
    async fn migrates_v2_category_storage_and_rejects_invalid_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("test.sqlite3");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                r#"
                CREATE TABLE users (
                    username TEXT PRIMARY KEY COLLATE NOCASE,
                    password_hash TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO users VALUES ('admin', 'existing-hash', 1);

                CREATE TABLE uploads (
                    id TEXT PRIMARY KEY,
                    stored_filename TEXT NOT NULL UNIQUE,
                    original_filename TEXT NOT NULL,
                    display_filename TEXT NOT NULL,
                    mime_type TEXT NOT NULL,
                    media_kind TEXT NOT NULL CHECK (media_kind IN ('image', 'video')),
                    size_bytes INTEGER NOT NULL CHECK (size_bytes >= 0),
                    original_size_bytes INTEGER NOT NULL CHECK (original_size_bytes >= 0),
                    converted INTEGER NOT NULL CHECK (converted IN (0, 1)),
                    width INTEGER,
                    height INTEGER,
                    latitude REAL,
                    longitude REAL,
                    capture_date TEXT,
                    author TEXT,
                    commons_category TEXT NOT NULL,
                    license TEXT NOT NULL,
                    uploader_username TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                INSERT INTO uploads VALUES (
                    'legacy', 'legacy.jpg', 'legacy.jpg', 'Legacy.jpg',
                    'image/jpeg', 'image', 10, 10, 0, 1, 1, NULL, NULL, NULL,
                    'Legacy Author', 'Minsk', 'cc-by-4.0', 'anonymous', 1
                );
                PRAGMA user_version = 2;
                "#,
            )
            .unwrap();
        drop(connection);

        let db = Database::new(&path);
        db.initialize().await.unwrap();
        assert_eq!(
            db.password_hash("admin").await.unwrap().as_deref(),
            Some("existing-hash")
        );
        let legacy = db.get_upload("legacy").await.unwrap().unwrap();
        assert_eq!(legacy.author.as_deref(), Some("Legacy Author"));
        assert_eq!(legacy.commons_categories, ["Minsk"]);

        let connection = rusqlite::Connection::open(&path).unwrap();
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 3);
        connection
            .execute(
                "UPDATE uploads SET commons_categories_json = '[]' WHERE id = 'legacy'",
                [],
            )
            .unwrap();
        drop(connection);
        assert!(db.get_upload("legacy").await.is_err());
    }
}
