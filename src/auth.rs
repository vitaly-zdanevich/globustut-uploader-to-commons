use crate::config::Config;
use crate::db::Database;
use anyhow::{Result, bail};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use axum::http::HeaderMap;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};

const SECURE_COOKIE_NAME: &str = "__Host-globustut_admin";
const LOCAL_COOKIE_NAME: &str = "globustut_admin";
const MAX_LOGIN_FAILURES: usize = 5;
const MAX_GLOBAL_UNKNOWN_ATTEMPTS: usize = 50;
const MAX_TRACKED_USERNAMES: usize = 64;
const MAX_CONCURRENT_PASSWORD_JOBS: usize = 2;
const LOGIN_WINDOW: Duration = Duration::from_secs(15 * 60);
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=65536,t=3,p=1$Z2xvYnVzdHV0LWR1bW15LXNhbHQ$liHFeSyVBC3nvVYtvAfs/5qpuHuJdL7XQMOG3fy9mvs";

static PASSWORD_WORK_SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

#[derive(Clone)]
pub struct AuthService {
    db: Database,
    session_ttl_seconds: i64,
    cookie_secure: bool,
    dummy_password_hash: Arc<String>,
    login_throttle: Arc<Mutex<LoginThrottle>>,
}

#[derive(Default)]
struct LoginThrottle {
    by_username: HashMap<String, VecDeque<Instant>>,
    unknown_attempts: VecDeque<Instant>,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedAdmin {
    pub username: String,
    pub csrf_token: String,
}

#[derive(Clone, Debug)]
pub struct LoginResult {
    pub username: String,
    pub set_cookie: String,
}

impl AuthService {
    pub fn new(config: &Config, db: Database) -> Result<Self> {
        Ok(Self {
            db,
            session_ttl_seconds: config.session_ttl_seconds,
            cookie_secure: config.cookie_secure,
            dummy_password_hash: Arc::new(DUMMY_PASSWORD_HASH.to_string()),
            login_throttle: Arc::new(Mutex::new(LoginThrottle::default())),
        })
    }

    pub async fn current_admin(&self, headers: &HeaderMap) -> Result<Option<AuthenticatedAdmin>> {
        let Some(raw_token) = self.cookie_token(headers) else {
            return Ok(None);
        };
        let Some(session) = self
            .db
            .get_session(hash_secret(&raw_token), now_ts())
            .await?
        else {
            return Ok(None);
        };
        if self.db.password_hash(&session.username).await?.is_none() {
            self.db.delete_session(hash_secret(&raw_token)).await?;
            return Ok(None);
        }
        Ok(Some(AuthenticatedAdmin {
            username: session.username,
            csrf_token: session.csrf_token,
        }))
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<LoginResult> {
        let username = normalize_username(username)?;
        let failure_key = username.to_lowercase();
        let stored_hash = self.db.password_hash(&username).await?;
        if !self
            .reserve_login_attempt(failure_key.clone(), stored_hash.is_none())
            .await
        {
            bail!("too many login attempts; try again later");
        }
        let candidate_hash = stored_hash
            .as_deref()
            .unwrap_or(self.dummy_password_hash.as_str());
        let valid = verify_password_async(password, candidate_hash).await?;
        if !valid || stored_hash.is_none() {
            bail!("invalid username or password");
        }
        let expected_password_hash = stored_hash.expect("stored hash checked above");

        let raw_token = random_secret(32);
        let csrf_token = random_secret(24);
        let now = now_ts();
        let session_created = self
            .db
            .insert_session_if_password_hash(
                hash_secret(&raw_token),
                username.clone(),
                csrf_token,
                now.saturating_add(self.session_ttl_seconds),
                now,
                expected_password_hash,
            )
            .await?;
        if !session_created {
            bail!("credentials changed during login; try again");
        }
        self.login_throttle
            .lock()
            .await
            .by_username
            .remove(&failure_key);
        Ok(LoginResult {
            username,
            set_cookie: self.session_cookie(&raw_token),
        })
    }

    pub async fn logout(&self, headers: &HeaderMap, csrf_token: &str) -> Result<String> {
        let Some(raw_token) = self.cookie_token(headers) else {
            return Ok(self.clear_cookie());
        };
        let token_hash = hash_secret(&raw_token);
        let Some(session) = self.db.get_session(token_hash.clone(), now_ts()).await? else {
            return Ok(self.clear_cookie());
        };
        if session.csrf_token != csrf_token {
            bail!("invalid CSRF token");
        }
        self.db.delete_session(token_hash).await?;
        Ok(self.clear_cookie())
    }

    pub fn validate_csrf(admin: &AuthenticatedAdmin, csrf_token: &str) -> Result<()> {
        if admin.csrf_token != csrf_token {
            bail!("invalid CSRF token");
        }
        Ok(())
    }

    async fn reserve_login_attempt(&self, key: String, unknown_username: bool) -> bool {
        self.login_throttle
            .lock()
            .await
            .reserve(key, unknown_username)
    }

    fn cookie_token(&self, headers: &HeaderMap) -> Option<String> {
        let name = self.cookie_name();
        headers
            .get_all(axum::http::header::COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|cookies| cookies.split(';'))
            .filter_map(|cookie| cookie.trim().split_once('='))
            .find_map(|(key, value)| (key == name).then(|| value.to_string()))
    }

    fn cookie_name(&self) -> &'static str {
        if self.cookie_secure {
            SECURE_COOKIE_NAME
        } else {
            LOCAL_COOKIE_NAME
        }
    }

    fn session_cookie(&self, token: &str) -> String {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        format!(
            "{}={token}; Path=/; HttpOnly{secure}; SameSite=Strict; Max-Age={}",
            self.cookie_name(),
            self.session_ttl_seconds
        )
    }

    fn clear_cookie(&self) -> String {
        let secure = if self.cookie_secure { "; Secure" } else { "" };
        format!(
            "{}=; Path=/; HttpOnly{secure}; SameSite=Strict; Max-Age=0",
            self.cookie_name()
        )
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    validate_new_password(password)?;
    let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| anyhow::anyhow!("failed to hash password: {error}"))
}

pub async fn hash_password_async(password: &str) -> Result<String> {
    validate_new_password(password)?;
    let password = password.to_string();
    run_password_job(move || hash_password(&password)).await?
}

fn verify_password(password: &str, encoded_hash: &str) -> bool {
    let Ok(hash) = PasswordHash::new(encoded_hash) else {
        return false;
    };
    argon2().verify_password(password.as_bytes(), &hash).is_ok()
}

async fn verify_password_async(password: &str, encoded_hash: &str) -> Result<bool> {
    let password = password.to_string();
    let encoded_hash = encoded_hash.to_string();
    run_password_job(move || verify_password(&password, &encoded_hash)).await
}

async fn run_password_job<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let slots = PASSWORD_WORK_SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_PASSWORD_JOBS)))
        .clone();
    let permit = slots
        .acquire_owned()
        .await
        .map_err(|_| anyhow::anyhow!("password worker limiter closed unexpectedly"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    })
    .await
    .map_err(|error| anyhow::anyhow!("password worker stopped unexpectedly: {error}"))
}

fn argon2() -> Argon2<'static> {
    // 64 MiB, three passes and one lane is deliberately expensive but fits Toolforge comfortably.
    let parameters = Params::new(64 * 1024, 3, 1, None).expect("valid Argon2 parameters");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters)
}

fn validate_new_password(password: &str) -> Result<()> {
    if password.chars().count() < 12 {
        bail!("password must contain at least 12 characters");
    }
    if password.len() > 1024 {
        bail!("password is too long");
    }
    Ok(())
}

fn normalize_username(username: &str) -> Result<String> {
    let username = username.trim();
    if username.is_empty() || username.len() > 100 || username.chars().any(char::is_control) {
        bail!("invalid username");
    }
    Ok(username.to_string())
}

fn random_secret(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn hash_secret(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

impl LoginThrottle {
    fn reserve(&mut self, key: String, unknown_username: bool) -> bool {
        self.prune();
        if self
            .by_username
            .get(&key)
            .is_some_and(|attempts| attempts.len() >= MAX_LOGIN_FAILURES)
        {
            return false;
        }
        if unknown_username && self.unknown_attempts.len() >= MAX_GLOBAL_UNKNOWN_ATTEMPTS {
            return false;
        }
        if !self.by_username.contains_key(&key) && self.by_username.len() >= MAX_TRACKED_USERNAMES {
            return false;
        }

        let now = Instant::now();
        self.by_username.entry(key).or_default().push_back(now);
        if unknown_username {
            self.unknown_attempts.push_back(now);
        }
        true
    }

    fn prune(&mut self) {
        self.by_username.retain(|_, attempts| {
            attempts.retain(|attempt| attempt.elapsed() < LOGIN_WINDOW);
            !attempts.is_empty()
        });
        self.unknown_attempts
            .retain(|attempt| attempt.elapsed() < LOGIN_WINDOW);
    }
}

pub fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::{
        AuthService, LoginThrottle, MAX_GLOBAL_UNKNOWN_ATTEMPTS, MAX_LOGIN_FAILURES,
        MAX_TRACKED_USERNAMES, hash_password, hash_password_async, verify_password,
    };
    use crate::config::Config;
    use crate::db::Database;

    #[test]
    fn hashes_and_verifies_passwords() {
        let encoded = hash_password("correct horse battery staple").unwrap();
        assert!(encoded.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &encoded));
        assert!(!verify_password("incorrect password", &encoded));
        assert!(hash_password("short").is_err());
    }

    #[tokio::test]
    async fn hashes_passwords_on_the_bounded_worker_pool() {
        let encoded = hash_password_async("correct horse battery staple")
            .await
            .unwrap();
        assert!(verify_password("correct horse battery staple", &encoded));
    }

    #[test]
    fn login_throttle_limits_each_username_and_unknown_names_globally() {
        let mut per_username = LoginThrottle::default();
        for _ in 0..MAX_LOGIN_FAILURES {
            assert!(per_username.reserve("moderator".into(), false));
        }
        assert!(!per_username.reserve("moderator".into(), false));

        let mut global = LoginThrottle::default();
        for attempt in 0..MAX_GLOBAL_UNKNOWN_ATTEMPTS {
            assert!(global.reserve(format!("unknown-{attempt}"), true));
        }
        assert!(!global.reserve("one-more-unknown".into(), true));
        assert_eq!(global.unknown_attempts.len(), MAX_GLOBAL_UNKNOWN_ATTEMPTS);
        assert!(global.by_username.len() <= MAX_TRACKED_USERNAMES);
    }

    #[test]
    fn login_throttle_bounds_username_storage() {
        let mut throttle = LoginThrottle::default();
        for attempt in 0..MAX_TRACKED_USERNAMES {
            assert!(throttle.reserve(format!("known-{attempt}"), false));
        }
        assert!(!throttle.reserve("known-overflow".into(), false));
        assert_eq!(throttle.by_username.len(), MAX_TRACKED_USERNAMES);
    }

    #[tokio::test]
    async fn login_creates_a_secure_session() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = Config::from_env().unwrap();
        config.database_path = directory.path().join("test.sqlite3");
        config.cookie_secure = false;
        let db = Database::new(&config.database_path);
        db.initialize().await.unwrap();
        db.create_user(
            "moderator_name",
            &hash_password_async("correct horse battery staple")
                .await
                .unwrap(),
            1,
        )
        .await
        .unwrap();
        let auth = AuthService::new(&config, db).unwrap();

        assert!(
            auth.login("moderator_name", "wrong password")
                .await
                .is_err()
        );
        let login = auth
            .login("moderator_name", "correct horse battery staple")
            .await
            .unwrap();
        assert_eq!(login.username, "moderator_name");
        assert!(login.set_cookie.contains("HttpOnly"));
        assert!(login.set_cookie.contains("SameSite=Strict"));
    }
}
