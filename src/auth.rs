//! Authentication, sessions, and administrator bootstrap state.

use std::{fmt, path::Path};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};
use getrandom::fill as fill_random;
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use thiserror::Error;
use tokio::fs;

const ADMIN_USERNAME: &str = "admin";
const BOOTSTRAP_PASSWORD_BYTES: usize = 18;
const SESSION_TOKEN_BYTES: usize = 32;
const MAX_USERNAME_BYTES: usize = 64;
const MIN_PASSWORD_BYTES: usize = 8;

/// Authentication and session storage backed by `SQLite`.
#[derive(Clone, Debug)]
pub struct AuthState {
    pool: SqlitePool,
}

/// Result of authentication bootstrap during application startup.
#[derive(Debug)]
pub struct BootstrapReport {
    password: Option<BootstrapPassword>,
}

/// Plaintext bootstrap password surfaced only during the accepted bootstrap window.
pub struct BootstrapPassword {
    username: String,
    plaintext_password: String,
    password_hash_for_debug: String,
}

/// A newly-created authenticated session.
#[derive(Debug)]
pub struct LoginSession {
    token: String,
}

/// Authenticated identity loaded from a session.
#[derive(Debug)]
pub struct Identity {
    username: String,
    is_admin: bool,
}

/// Authentication subsystem errors.
#[derive(Debug, Error)]
pub enum AuthError {
    /// `SQLite` failed.
    #[error("authentication database operation failed")]
    Database(#[from] sqlx::Error),
    /// `SQLite` parent directory could not be created.
    #[error("authentication database parent directory could not be created")]
    DatabaseDirectory(#[source] std::io::Error),
    /// Secure random generation failed.
    #[error("secure random generation failed")]
    Random(#[from] getrandom::Error),
    /// Password hashing failed.
    #[error("password hashing failed")]
    PasswordHash(String),
    /// Password verification failed.
    #[error("password verification failed")]
    PasswordVerify(String),
    /// Username is invalid.
    #[error("username is invalid")]
    InvalidUsername,
    /// Password is invalid.
    #[error("password is invalid")]
    InvalidPassword,
    /// The fixed Administrator cannot be created as an ordinary user.
    #[error("administrator cannot be created as an ordinary user")]
    ReservedAdministrator,
    /// A blocking password task panicked or was cancelled.
    #[error("password task failed")]
    PasswordTask(#[from] tokio::task::JoinError),
}

impl AuthState {
    /// Initialize `SQLite`-backed authentication and bootstrap the fixed Administrator.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite` cannot be opened, migrations fail, secure random generation
    /// fails, or password hashing fails.
    pub async fn initialize(database_path: &Path) -> Result<(Self, BootstrapReport), AuthError> {
        let pool = connect_pool(database_path).await?;
        run_migrations(&pool).await?;

        let state = Self { pool };
        let password = state.bootstrap_administrator().await?;
        Ok((state, BootstrapReport { password }))
    }

    /// Connect to an existing `SQLite`-backed authentication store.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite` cannot be opened or migrations fail.
    pub async fn connect_existing(database_path: &Path) -> Result<Self, AuthError> {
        let pool = connect_pool(database_path).await?;
        run_migrations(&pool).await?;
        Ok(Self { pool })
    }

    /// Create an ordinary User with an initial Password.
    ///
    /// This storage-level capability is used by the later Console slice; it intentionally does
    /// not expose HTTP Console user management in this issue.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid credentials, reserved Administrator username, `SQLite` failure,
    /// or password hashing failure.
    pub async fn create_user(&self, username: &str, password: &str) -> Result<(), AuthError> {
        if !is_valid_username(username) {
            return Err(AuthError::InvalidUsername);
        }
        if normalize_username(username) == ADMIN_USERNAME {
            return Err(AuthError::ReservedAdministrator);
        }
        if !is_valid_password(password) {
            return Err(AuthError::InvalidPassword);
        }

        let password_hash = hash_password(password.to_owned()).await?;
        sqlx::query(
            "INSERT INTO users (username, username_normalized, password_hash, is_admin, \
             bootstrap_pending) VALUES (?, ?, ?, 0, 0)",
        )
        .bind(username)
        .bind(normalize_username(username))
        .bind(password_hash)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn bootstrap_administrator(&self) -> Result<Option<BootstrapPassword>, AuthError> {
        let admin = sqlx::query_as::<_, (i64, String, i64)>(
            "SELECT id, password_hash, bootstrap_pending FROM users WHERE username_normalized = ?",
        )
        .bind(ADMIN_USERNAME)
        .fetch_optional(&self.pool)
        .await?;

        match admin {
            Some((_admin_id, _password_hash, 0)) => Ok(None),
            Some((admin_id, _password_hash, _bootstrap_pending)) => {
                let password = generate_secret_hex(BOOTSTRAP_PASSWORD_BYTES)?;
                let password_hash = hash_password(password.clone()).await?;
                sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
                    .bind(&password_hash)
                    .bind(admin_id)
                    .execute(&self.pool)
                    .await?;
                sqlx::query("DELETE FROM sessions WHERE user_id = ?")
                    .bind(admin_id)
                    .execute(&self.pool)
                    .await?;
                Ok(Some(BootstrapPassword::new(password, password_hash)))
            }
            None => {
                let password = generate_secret_hex(BOOTSTRAP_PASSWORD_BYTES)?;
                let password_hash = hash_password(password.clone()).await?;
                sqlx::query(
                    "INSERT INTO users (username, username_normalized, password_hash, is_admin, \
                     bootstrap_pending) VALUES (?, ?, ?, 1, 1)",
                )
                .bind(ADMIN_USERNAME)
                .bind(ADMIN_USERNAME)
                .bind(&password_hash)
                .execute(&self.pool)
                .await?;
                Ok(Some(BootstrapPassword::new(password, password_hash)))
            }
        }
    }

    /// Authenticate a user and create a `SQLite`-backed session.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite`, password verification, or secure random generation fails.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<LoginSession>, AuthError> {
        if !is_valid_username(username) || !is_valid_password(password) {
            return Ok(None);
        }

        let Some((user_id, _username, password_hash, _is_admin)) =
            sqlx::query_as::<_, (i64, String, String, i64)>(
                "SELECT id, username, password_hash, is_admin FROM users WHERE \
                 username_normalized = ?",
            )
            .bind(normalize_username(username))
            .fetch_optional(&self.pool)
            .await?
        else {
            return Ok(None);
        };

        if !verify_password(password.to_owned(), password_hash).await? {
            return Ok(None);
        }

        let token = generate_secret_hex(SESSION_TOKEN_BYTES)?;
        sqlx::query("INSERT INTO sessions (token, user_id) VALUES (?, ?)")
            .bind(&token)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        Ok(Some(LoginSession { token }))
    }

    /// Load the authenticated identity for a session token.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite` fails.
    pub async fn identity_for_session(
        &self,
        session_token: Option<&str>,
    ) -> Result<Option<Identity>, AuthError> {
        let Some(session_token) = session_token else {
            return Ok(None);
        };
        if !is_valid_session_token(session_token) {
            return Ok(None);
        }

        let identity = sqlx::query_as::<_, (String, i64)>(
            "SELECT users.username, users.is_admin FROM sessions JOIN users ON users.id = \
             sessions.user_id WHERE sessions.token = ?",
        )
        .bind(session_token)
        .fetch_optional(&self.pool)
        .await?
        .map(|(username, is_admin)| Identity {
            username,
            is_admin: is_admin != 0,
        });

        Ok(identity)
    }

    /// Revoke one authenticated session token.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite` fails.
    pub async fn logout(&self, session_token: Option<&str>) -> Result<(), AuthError> {
        let Some(session_token) = session_token else {
            return Ok(());
        };
        if !is_valid_session_token(session_token) {
            return Ok(());
        }

        sqlx::query("DELETE FROM sessions WHERE token = ?")
            .bind(session_token)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Change the authenticated user's password and revoke all of that user's sessions.
    ///
    /// # Errors
    ///
    /// Returns an error if `SQLite`, password verification, or password hashing fails.
    pub async fn change_password(
        &self,
        session_token: Option<&str>,
        old_password: &str,
        new_password: &str,
    ) -> Result<bool, AuthError> {
        let Some(session_token) = session_token else {
            return Ok(false);
        };
        if !is_valid_session_token(session_token)
            || !is_valid_password(old_password)
            || !is_valid_password(new_password)
        {
            return Ok(false);
        }

        let Some((user_id, password_hash)) = sqlx::query_as::<_, (i64, String)>(
            "SELECT users.id, users.password_hash FROM sessions JOIN users ON users.id = \
             sessions.user_id WHERE sessions.token = ?",
        )
        .bind(session_token)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(false);
        };

        if !verify_password(old_password.to_owned(), password_hash).await? {
            return Ok(false);
        }

        let new_hash = hash_password(new_password.to_owned()).await?;
        sqlx::query("UPDATE users SET password_hash = ?, bootstrap_pending = 0 WHERE id = ?")
            .bind(new_hash)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE user_id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        Ok(true)
    }
}

impl BootstrapReport {
    /// Return the bootstrap password if startup is still in the Administrator bootstrap window.
    #[must_use]
    pub const fn bootstrap_password(&self) -> Option<&BootstrapPassword> {
        self.password.as_ref()
    }
}

impl BootstrapPassword {
    fn new(plaintext_password: String, password_hash_for_debug: String) -> Self {
        Self {
            username: ADMIN_USERNAME.to_owned(),
            plaintext_password,
            password_hash_for_debug,
        }
    }

    /// Return the fixed Administrator username.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Return the generated plaintext bootstrap password.
    #[must_use]
    pub fn plaintext_password(&self) -> &str {
        &self.plaintext_password
    }

    /// Return the persisted password hash string for tests that verify plaintext is not embedded.
    #[must_use]
    pub fn password_hash_for_debug(&self) -> &str {
        &self.password_hash_for_debug
    }
}

impl LoginSession {
    /// Return the bearer session token.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Identity {
    /// Return the display Username.
    #[must_use]
    pub fn username(&self) -> &str {
        &self.username
    }

    /// Return whether this identity is the fixed Administrator.
    #[must_use]
    pub const fn is_admin(&self) -> bool {
        self.is_admin
    }
}

impl fmt::Debug for BootstrapPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootstrapPassword")
            .field("username", &self.username)
            .field("plaintext_password", &"<redacted>")
            .field("password_hash_for_debug", &"<redacted>")
            .finish()
    }
}

async fn run_migrations(pool: &SqlitePool) -> Result<(), AuthError> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username TEXT NOT NULL,
            username_normalized TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            is_admin INTEGER NOT NULL DEFAULT 0,
            bootstrap_pending INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE
        )",
    )
    .execute(pool)
    .await?;

    Ok(())
}

async fn connect_pool(database_path: &Path) -> Result<SqlitePool, AuthError> {
    if let Some(parent) = database_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(AuthError::DatabaseDirectory)?;
    }

    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(true);
    Ok(SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?)
}

async fn hash_password(password: String) -> Result<String, AuthError> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|error| AuthError::PasswordHash(error.to_string()))
    })
    .await?
}

#[allow(dead_code)]
async fn verify_password(password: String, password_hash: String) -> Result<bool, AuthError> {
    tokio::task::spawn_blocking(move || {
        let parsed_hash = PasswordHash::new(&password_hash)
            .map_err(|error| AuthError::PasswordVerify(error.to_string()))?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    })
    .await?
}

fn generate_secret_hex(byte_count: usize) -> Result<String, AuthError> {
    let mut bytes = vec![0u8; byte_count];
    fill_random(&mut bytes)?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(hex_digit(byte >> 4));
        encoded.push(hex_digit(byte & 0x0F));
    }
    encoded
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'a' + (value - 10)),
        _ => '0',
    }
}

fn is_valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= MAX_USERNAME_BYTES
        && username
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn normalize_username(username: &str) -> String {
    username.to_ascii_lowercase()
}

fn is_valid_password(password: &str) -> bool {
    password.len() >= MIN_PASSWORD_BYTES
}

fn is_valid_session_token(session_token: &str) -> bool {
    session_token.len() == SESSION_TOKEN_BYTES * 2
        && session_token.bytes().all(|byte| byte.is_ascii_hexdigit())
}
