//! HTTP routes for File Hub.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequestParts, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::io::ReaderStream;
use tower::ServiceBuilder;
use tower_http::timeout::TimeoutLayer;
use tracing::warn;

use crate::{
    auth::{AuthError, AuthState, BootstrapPassword, BootstrapReport, ManagedUser, PermissionSet},
    config::AppConfig,
    resource,
};

const SESSION_COOKIE_NAME: &str = "fh_session";

/// Build the public HTTP router.
///
/// # Errors
///
/// Returns an error when authentication/session storage cannot be initialized.
pub async fn build_router(config: AppConfig) -> Result<Router, HttpInitError> {
    Ok(build_router_with_bootstrap_report(config)
        .await?
        .into_router())
}

/// Build the public HTTP router and return startup bootstrap information.
///
/// # Errors
///
/// Returns an error when authentication/session storage cannot be initialized.
pub async fn build_router_with_bootstrap_report(
    config: AppConfig,
) -> Result<RouterWithBootstrapReport, HttpInitError> {
    let timeout = std::time::Duration::from_secs(
        config
            .limits()
            .request_timeout_seconds()
            .get()
            .try_into()
            .map_or(300, |value| value),
    );
    let (auth, bootstrap_report) = AuthState::initialize(config.database_path()).await?;
    log_bootstrap_password(&bootstrap_report);
    let state = AppState {
        config: Arc::new(config),
        auth,
    };

    let router = Router::new()
        .route("/", get(index))
        .route("/api/identity", get(identity))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/password", post(change_password))
        .route("/console", get(console_view))
        .route(
            "/api/console/users",
            get(console_list_users).post(console_create_user),
        )
        .route(
            "/api/console/anonymous-permissions",
            get(console_get_anonymous_permissions).patch(console_update_anonymous_permissions),
        )
        .route(
            "/api/console/users/{username}",
            delete(console_delete_user)
                .patch(console_rename_user)
                .put(console_replace_user),
        )
        .route(
            "/api/console/users/{username}/permissions",
            patch(console_update_user_permissions),
        )
        .route(
            "/api/console/users/{username}/password",
            post(console_reset_user_password),
        )
        .route("/api/list", get(list_root_directory))
        .route("/api/search", get(search_resources))
        .route("/api/download", get(download_file))
        .route("/api/archive", get(download_directory_archive))
        .with_state(state)
        .layer(ServiceBuilder::new().layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        )));

    Ok(RouterWithBootstrapReport {
        router,
        bootstrap_report,
    })
}

/// Router plus Administrator bootstrap information from startup.
#[derive(Debug)]
pub struct RouterWithBootstrapReport {
    router: Router,
    bootstrap_report: BootstrapReport,
}

/// HTTP router initialization errors.
#[derive(Debug, Error)]
pub enum HttpInitError {
    /// Authentication/session storage failed to initialize.
    #[error("authentication storage initialization failed")]
    Auth(#[from] AuthError),
}

impl RouterWithBootstrapReport {
    /// Consume this value and return the HTTP router.
    pub fn into_router(self) -> Router {
        self.router
    }

    /// Return the Administrator bootstrap password if startup is still in the bootstrap window.
    #[must_use]
    pub const fn bootstrap_password(&self) -> Option<&BootstrapPassword> {
        self.bootstrap_report.bootstrap_password()
    }
}

#[derive(Clone, Debug)]
struct AppState {
    config: Arc<AppConfig>,
    auth: AuthState,
}

#[derive(Debug)]
struct Administrator;

impl FromRequestParts<AppState> for Administrator {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        require_admin(state, &parts.headers).await?;
        Ok(Self)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    path: Option<String>,
    sort: Option<resource::SortField>,
    order: Option<resource::SortOrder>,
    filter: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadQuery {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchQuery {
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LoginRequest {
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PasswordChangeRequest {
    old_password: String,
    new_password: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsoleUserRequest {
    username: String,
    password: String,
    permissions: Option<PermissionSetBody>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsolePasswordRequest {
    password: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConsoleRenameUserRequest {
    username: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PermissionSetBody {
    upload: bool,
    rename: bool,
    delete: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityResponse {
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    username: Option<String>,
    actions: IdentityActions,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
struct IdentityActions {
    login: bool,
    password_change: bool,
    logout: bool,
    console: bool,
    upload: bool,
    rename: bool,
    delete: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConsoleUsersResponse {
    users: Vec<ManagedUserResponse>,
    anonymous_permissions: PermissionSetBody,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedUserResponse {
    username: String,
    permissions: PermissionSetBody,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorBody {
    code: &'static str,
    reason: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    reason: String,
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn identity(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<IdentityResponse>, ApiError> {
    let token = session_token_from_headers(&headers);
    let identity = state.auth.identity_for_session(token.as_deref()).await?;

    Ok(Json(if let Some(identity) = identity {
        let permissions = identity.permissions();
        IdentityResponse {
            authenticated: true,
            username: Some(identity.username().to_owned()),
            actions: IdentityActions {
                login: false,
                password_change: true,
                logout: true,
                console: identity.is_admin(),
                upload: permissions.upload(),
                rename: permissions.rename(),
                delete: permissions.delete(),
            },
        }
    } else {
        let permissions = state.auth.anonymous_permissions().await?;
        IdentityResponse {
            authenticated: false,
            username: None,
            actions: IdentityActions {
                login: true,
                password_change: false,
                logout: false,
                console: false,
                upload: permissions.upload(),
                rename: permissions.rename(),
                delete: permissions.delete(),
            },
        }
    }))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Response, ApiError> {
    let session = state
        .auth
        .login(&request.username, &request.password)
        .await?
        .ok_or_else(ApiError::invalid_credentials)?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, session_cookie_header(session.token())?);
    Ok(response)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let token = session_token_from_headers(&headers);
    state.auth.logout(token.as_deref()).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, expired_session_cookie_header());
    Ok(response)
}

async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PasswordChangeRequest>,
) -> Result<Response, ApiError> {
    let token = session_token_from_headers(&headers);
    if !state
        .auth
        .change_password(
            token.as_deref(),
            &request.old_password,
            &request.new_password,
        )
        .await?
    {
        return Err(ApiError::invalid_credentials());
    }

    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, expired_session_cookie_header());
    Ok(response)
}

async fn console_view(_administrator: Administrator) -> Result<Html<&'static str>, ApiError> {
    Ok(Html(CONSOLE_HTML))
}

async fn console_list_users(
    State(state): State<AppState>,
    _administrator: Administrator,
) -> Result<Json<ConsoleUsersResponse>, ApiError> {
    let users = state
        .auth
        .list_users()
        .await?
        .iter()
        .map(ManagedUserResponse::from)
        .collect();
    let anonymous_permissions = PermissionSetBody::from(state.auth.anonymous_permissions().await?);
    Ok(Json(ConsoleUsersResponse {
        users,
        anonymous_permissions,
    }))
}

async fn console_create_user(
    State(state): State<AppState>,
    _administrator: Administrator,
    Json(request): Json<ConsoleUserRequest>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&request.username)?;
    let permissions = request
        .permissions
        .map(PermissionSet::from)
        .unwrap_or_default();
    state
        .auth
        .create_user_with_permissions(&request.username, &request.password, permissions)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(ManagedUserResponse {
            username: request.username,
            permissions: PermissionSetBody::from(permissions),
        }),
    )
        .into_response())
}

async fn console_delete_user(
    State(state): State<AppState>,
    _administrator: Administrator,
    AxumPath(username): AxumPath<String>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&username)?;
    state.auth.delete_user(&username).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn console_rename_user(
    State(state): State<AppState>,
    _administrator: Administrator,
    AxumPath(username): AxumPath<String>,
    Json(request): Json<ConsoleRenameUserRequest>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&username)?;
    reject_administrator_target(&request.username)?;
    let user = state.auth.rename_user(&username, &request.username).await?;
    Ok(Json(ManagedUserResponse::from(&user)).into_response())
}

async fn console_replace_user(
    State(state): State<AppState>,
    _administrator: Administrator,
    AxumPath(username): AxumPath<String>,
    Json(request): Json<ConsoleUserRequest>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&username)?;
    reject_administrator_target(&request.username)?;
    let permissions = request
        .permissions
        .map(PermissionSet::from)
        .unwrap_or_default();
    let user = state
        .auth
        .replace_user(&username, &request.username, &request.password, permissions)
        .await?;
    Ok(Json(ManagedUserResponse::from(&user)).into_response())
}

async fn console_update_user_permissions(
    State(state): State<AppState>,
    _administrator: Administrator,
    AxumPath(username): AxumPath<String>,
    Json(request): Json<PermissionSetBody>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&username)?;
    let user = state
        .auth
        .update_user_permissions(&username, PermissionSet::from(request))
        .await?;
    Ok(Json(ManagedUserResponse::from(&user)).into_response())
}

async fn console_reset_user_password(
    State(state): State<AppState>,
    _administrator: Administrator,
    AxumPath(username): AxumPath<String>,
    Json(request): Json<ConsolePasswordRequest>,
) -> Result<Response, ApiError> {
    reject_administrator_target(&username)?;
    state
        .auth
        .reset_user_password(&username, &request.password)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn console_get_anonymous_permissions(
    State(state): State<AppState>,
    _administrator: Administrator,
) -> Result<Json<PermissionSetBody>, ApiError> {
    Ok(Json(PermissionSetBody::from(
        state.auth.anonymous_permissions().await?,
    )))
}

async fn console_update_anonymous_permissions(
    State(state): State<AppState>,
    _administrator: Administrator,
    Json(request): Json<PermissionSetBody>,
) -> Result<Json<PermissionSetBody>, ApiError> {
    let permissions = state
        .auth
        .set_anonymous_permissions(PermissionSet::from(request))
        .await?;
    Ok(Json(PermissionSetBody::from(permissions)))
}

async fn require_admin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = session_token_from_headers(headers);
    let identity = state.auth.identity_for_session(token.as_deref()).await?;
    match identity {
        Some(identity) if identity.is_admin() => Ok(()),
        Some(_identity) => Err(ApiError::forbidden()),
        None => Err(ApiError::authentication_required()),
    }
}

fn reject_administrator_target(username: &str) -> Result<(), ApiError> {
    if username.eq_ignore_ascii_case("admin") {
        Err(ApiError::reserved_administrator())
    } else {
        Ok(())
    }
}

async fn list_root_directory(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Result<Json<resource::DirectoryListing>, ApiError> {
    let path = query.path.as_deref().unwrap_or_default();
    let sort = resource::ListingSort {
        field: query.sort.unwrap_or(resource::SortField::Name),
        order: query.order.unwrap_or(resource::SortOrder::Asc),
    };
    let filter = resource::CurrentListFilter {
        query: query.filter.unwrap_or_default(),
    };
    resource::list_directory(&state.config, path, sort, filter)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

async fn download_file(
    State(state): State<AppState>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let path = query
        .path
        .as_deref()
        .ok_or(resource::ResourceError::InvalidResourcePath)?;
    let download = resource::download_file(&state.config, path).await?;
    let content_disposition = content_disposition_header(&download.download_name)?;
    let content_length = HeaderValue::from_str(&download.content_length.to_string())
        .map_err(|_| ApiError::internal_server_error())?;

    let stream = ReaderStream::new(download.content);
    let mut response = Body::from_stream(stream).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::CONTENT_DISPOSITION, content_disposition);
    headers.insert(header::CONTENT_LENGTH, content_length);
    Ok(response)
}

async fn search_resources(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<resource::SearchResults>, ApiError> {
    let query = query
        .q
        .as_deref()
        .ok_or(resource::ResourceError::InvalidSearchQuery)?;
    resource::search_resources(&state.config, query)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

async fn download_directory_archive(
    State(state): State<AppState>,
    Query(query): Query<DownloadQuery>,
) -> Result<Response, ApiError> {
    let path = query
        .path
        .as_deref()
        .ok_or(resource::ResourceError::InvalidResourcePath)?;
    let download = resource::download_directory_archive(&state.config, path).await?;
    let content_disposition = content_disposition_header(&download.download_name)?;
    let content_length = HeaderValue::from_str(&download.content_length.to_string())
        .map_err(|_| ApiError::internal_server_error())?;

    let mut response = Body::from(download.content).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    headers.insert(header::CONTENT_DISPOSITION, content_disposition);
    headers.insert(header::CONTENT_LENGTH, content_length);
    Ok(response)
}

impl From<resource::ResourceError> for ApiError {
    fn from(error: resource::ResourceError) -> Self {
        match error {
            resource::ResourceError::InvalidResourcePath => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_resource_path",
                reason: "resource path is invalid".to_owned(),
            },
            resource::ResourceError::InvalidFilter => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_filter",
                reason: "current list filter query is invalid".to_owned(),
            },
            resource::ResourceError::InvalidSearchQuery => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_search_query",
                reason: "server search query is invalid".to_owned(),
            },
            resource::ResourceError::NotDirectory => Self {
                status: StatusCode::BAD_REQUEST,
                code: "not_directory",
                reason: "resource path is not a directory".to_owned(),
            },
            resource::ResourceError::NotFile => Self {
                status: StatusCode::BAD_REQUEST,
                code: "not_file",
                reason: "resource path is not a file".to_owned(),
            },
            resource::ResourceError::RootDirectoryArchive => Self {
                status: StatusCode::BAD_REQUEST,
                code: "root_directory_archive",
                reason: "Root Directory cannot be downloaded as an archive".to_owned(),
            },
            resource::ResourceError::ResourceNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "resource_not_found",
                reason: "resource path does not exist".to_owned(),
            },
            resource::ResourceError::ListingLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "listing_limit_exceeded",
                reason: format!("direct child listing exceeds configured limit of {limit}"),
            },
            resource::ResourceError::ArchiveResourceCountLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "archive_resource_count_limit_exceeded",
                reason: format!(
                    "directory archive exceeds configured resource count limit of {limit}"
                ),
            },
            resource::ResourceError::ArchiveSizeLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "archive_size_limit_exceeded",
                reason: format!(
                    "directory archive exceeds configured uncompressed size limit of {limit} bytes",
                ),
            },
            resource::ResourceError::InvalidResourceName => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "invalid_resource_name",
                reason: "resource name is not valid UTF-8".to_owned(),
            },
            resource::ResourceError::ReadDirectory(_)
            | resource::ResourceError::ReadEntry(_)
            | resource::ResourceError::Metadata(_)
            | resource::ResourceError::ModifiedTime(_)
            | resource::ResourceError::ReadFile(_)
            | resource::ResourceError::ReadArchiveFile(_)
            | resource::ResourceError::ZipArchive(_)
            | resource::ResourceError::WriteArchive(_)
            | resource::ResourceError::ArchiveLengthOverflow => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "resource_listing_failed",
                reason: "failed to read Resource".to_owned(),
            },
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::InvalidUsername => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_username",
                reason: "username is invalid".to_owned(),
            },
            AuthError::InvalidPassword => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_password",
                reason: "password is invalid".to_owned(),
            },
            AuthError::ReservedAdministrator => Self::reserved_administrator(),
            AuthError::UsernameConflict => Self {
                status: StatusCode::CONFLICT,
                code: "username_conflict",
                reason: "username already exists".to_owned(),
            },
            AuthError::UserNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "user_not_found",
                reason: "user was not found".to_owned(),
            },
            AuthError::Database(_)
            | AuthError::DatabaseDirectory(_)
            | AuthError::Random(_)
            | AuthError::PasswordHash(_)
            | AuthError::PasswordVerify(_)
            | AuthError::PasswordTask(_) => Self::internal_server_error(),
        }
    }
}

impl From<PermissionSetBody> for PermissionSet {
    fn from(value: PermissionSetBody) -> Self {
        Self::new(value.upload, value.rename, value.delete)
    }
}

impl From<PermissionSet> for PermissionSetBody {
    fn from(value: PermissionSet) -> Self {
        Self {
            upload: value.upload(),
            rename: value.rename(),
            delete: value.delete(),
        }
    }
}

impl From<&ManagedUser> for ManagedUserResponse {
    fn from(value: &ManagedUser) -> Self {
        Self {
            username: value.username().to_owned(),
            permissions: PermissionSetBody::from(value.permissions()),
        }
    }
}

impl ApiError {
    fn internal_server_error() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_server_error",
            reason: "internal server error".to_owned(),
        }
    }

    fn invalid_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_credentials",
            reason: "username or password is invalid".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "authentication_required",
            reason: "authentication is required".to_owned(),
        }
    }

    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            reason: "administrator identity is required".to_owned(),
        }
    }

    fn reserved_administrator() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "reserved_administrator",
            reason: "Administrator cannot be created, deleted, renamed, or replaced".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code,
                    reason: self.reason,
                },
            }),
        )
            .into_response()
    }
}

fn content_disposition_header(download_name: &str) -> Result<HeaderValue, ApiError> {
    let value = format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        quoted_filename(download_name),
        percent_encode_filename(download_name),
    );
    HeaderValue::from_str(&value).map_err(|_| ApiError::internal_server_error())
}

fn quoted_filename(download_name: &str) -> String {
    let mut quoted = String::with_capacity(download_name.len());
    for character in download_name.chars() {
        if character == '"' || character == '\\' {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted
}

fn percent_encode_filename(download_name: &str) -> String {
    let mut encoded = String::with_capacity(download_name.len());
    for byte in download_name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(upper_hex_digit(byte >> 4));
            encoded.push(upper_hex_digit(byte & 0x0F));
        }
    }
    encoded
}

fn upper_hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'A' + (value - 10)),
        _ => '%',
    }
}

fn session_cookie_header(token: &str) -> Result<HeaderValue, ApiError> {
    let value = format!("{SESSION_COOKIE_NAME}={token}; Path=/; HttpOnly; Secure; SameSite=Lax");
    HeaderValue::from_str(&value).map_err(|_| ApiError::internal_server_error())
}

fn expired_session_cookie_header() -> HeaderValue {
    HeaderValue::from_static("fh_session=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
}

fn session_token_from_headers(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for cookie in header.split(';') {
        let cookie = cookie.trim();
        let Some(value) = cookie.strip_prefix(SESSION_COOKIE_NAME) else {
            continue;
        };
        let Some(value) = value.strip_prefix('=') else {
            continue;
        };
        return Some(value.to_owned());
    }
    None
}

fn log_bootstrap_password(report: &BootstrapReport) {
    if let Some(password) = report.bootstrap_password() {
        warn!(
            username = password.username(),
            bootstrap_password = password.plaintext_password(),
            "Administrator bootstrap password is active",
        );
    }
}

const CONSOLE_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>File Hub Console</title>
  <style>
    :root { color-scheme: light; font-family: ui-sans-serif, system-ui, sans-serif; color: #202124; background: #f5f6f7; }
    * { box-sizing: border-box; }
    body { margin: 0; }
    header { display: flex; align-items: center; justify-content: space-between; min-height: 56px; padding: 0 24px; background: #fff; border-bottom: 1px solid #d9dde3; }
    header h1 { margin: 0; font-size: 18px; }
    header a { color: #1769aa; text-decoration: none; }
    main { width: min(1100px, 100%); margin: 0 auto; padding: 24px; }
    section { margin-bottom: 28px; }
    h2 { margin: 0 0 12px; font-size: 16px; }
    form, .anonymous-permissions { display: flex; flex-wrap: wrap; align-items: end; gap: 12px; padding: 16px; background: #fff; border: 1px solid #d9dde3; border-radius: 6px; }
    label { display: grid; gap: 6px; font-size: 13px; }
    .permission { display: flex; align-items: center; gap: 6px; min-height: 36px; }
    input[type="text"], input[type="password"] { width: 210px; min-height: 36px; padding: 7px 9px; border: 1px solid #aeb4bd; border-radius: 4px; font: inherit; }
    button { min-height: 36px; padding: 7px 12px; border: 1px solid #8b929c; border-radius: 4px; background: #fff; color: inherit; font: inherit; cursor: pointer; }
    button.primary { border-color: #1769aa; background: #1769aa; color: #fff; }
    button.danger { border-color: #b3261e; color: #b3261e; }
    button:disabled { cursor: wait; opacity: .6; }
    dialog { width: min(420px, calc(100% - 28px)); padding: 20px; border: 1px solid #aeb4bd; border-radius: 6px; }
    dialog::backdrop { background: rgb(0 0 0 / .35); }
    dialog form { padding: 0; border: 0; }
    .dialog-actions { display: flex; justify-content: flex-end; gap: 8px; width: 100%; }
    .table-wrap { overflow-x: auto; background: #fff; border: 1px solid #d9dde3; border-radius: 6px; }
    table { width: 100%; border-collapse: collapse; }
    th, td { padding: 11px 12px; border-bottom: 1px solid #e3e6ea; text-align: left; white-space: nowrap; }
    th { font-size: 12px; color: #5f6368; background: #fafbfc; }
    tbody tr:last-child td { border-bottom: 0; }
    td.actions { display: flex; gap: 8px; }
    #console-status { min-height: 22px; margin: 0 0 12px; color: #5f6368; }
    #console-status.error { color: #b3261e; }
    .empty { color: #5f6368; text-align: center; }
    @media (max-width: 640px) { header, main { padding-left: 14px; padding-right: 14px; } input[type="text"], input[type="password"] { width: 100%; } form { align-items: stretch; } form > label { flex: 1 1 100%; } }
  </style>
</head>
<body>
  <header><h1>File Hub Console</h1><a href="/">Back to files</a></header>
  <main aria-label="Console">
    <p id="console-status" role="status" aria-live="polite"></p>
    <section aria-label="User Management">
      <h2>Users</h2>
      <form id="create-user-form">
        <label>Username <input id="console-username" name="username" type="text" autocomplete="off" maxlength="64" required pattern="[A-Za-z0-9_-]{1,64}"></label>
        <label>Initial password <input id="console-password" name="password" type="password" minlength="8" maxlength="256" required></label>
        <label><input id="console-upload-permission" name="upload" type="checkbox"> Upload Permission</label>
        <label><input id="console-rename-permission" name="rename" type="checkbox"> Rename Permission</label>
        <label><input id="console-delete-permission" name="delete" type="checkbox"> Delete Permission</label>
        <button class="primary" type="submit" id="create-user-action">Create User</button>
      </form>
      <div class="table-wrap">
        <table>
          <thead><tr><th>Username</th><th>Upload</th><th>Rename</th><th>Delete</th><th>Actions</th></tr></thead>
          <tbody id="console-users"></tbody>
        </table>
      </div>
    </section>
    <section aria-label="Default Anonymous Permission Set">
      <h2>Anonymous permissions</h2>
      <div class="anonymous-permissions">
        <label class="permission"><input id="anonymous-upload-permission" type="checkbox"> Upload Permission</label>
        <label class="permission"><input id="anonymous-rename-permission" type="checkbox"> Rename Permission</label>
        <label class="permission"><input id="anonymous-delete-permission" type="checkbox"> Delete Permission</label>
        <button class="primary" type="button" id="save-anonymous-permissions">Save</button>
      </div>
    </section>
    <dialog id="reset-password-dialog">
      <form id="reset-password-form">
        <h2>Reset password</h2>
        <label>New password <input id="reset-password-value" name="password" type="password" minlength="8" maxlength="256" required></label>
        <div class="dialog-actions">
          <button type="button" id="cancel-password-reset">Cancel</button>
          <button class="primary" type="submit">Reset password</button>
        </div>
      </form>
    </dialog>
  </main>
  <script>
    const statusArea = document.querySelector('#console-status');
    const usersBody = document.querySelector('#console-users');

    function setStatus(message, isError = false) {
      statusArea.textContent = message;
      statusArea.classList.toggle('error', isError);
    }

    async function api(path, options = {}) {
      const response = await fetch(path, {
        ...options,
        headers: options.body ? { 'Content-Type': 'application/json' } : undefined,
      });
      if (!response.ok) {
        const body = await response.json().catch(() => null);
        throw new Error(body?.error?.reason ?? `Request failed (${response.status})`);
      }
      return response.status === 204 ? null : response.json();
    }

    function permissionCheckbox(username, permission, checked) {
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.checked = checked;
      input.setAttribute('aria-label', `${username} ${permission} permission`);
      input.addEventListener('change', async () => {
        input.disabled = true;
        try {
          const row = input.closest('tr');
          const permissions = Object.fromEntries(
            [...row.querySelectorAll('input[type="checkbox"]')].map(item => [item.dataset.permission, item.checked]),
          );
          await api(`/api/console/users/${encodeURIComponent(username)}/permissions`, {
            method: 'PATCH', body: JSON.stringify(permissions),
          });
          setStatus(`Updated permissions for ${username}.`);
        } catch (error) {
          input.checked = !input.checked;
          setStatus(error.message, true);
        } finally {
          input.disabled = false;
        }
      });
      input.dataset.permission = permission;
      return input;
    }

    function actionButton(label, className, action) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = label;
      if (className) button.className = className;
      button.addEventListener('click', action);
      return button;
    }

    function renderUsers(users) {
      usersBody.replaceChildren();
      if (users.length === 0) {
        const cell = document.createElement('td');
        cell.colSpan = 5;
        cell.className = 'empty';
        cell.textContent = 'No ordinary users';
        const row = document.createElement('tr');
        row.append(cell);
        usersBody.append(row);
        return;
      }
      for (const user of users) {
        const row = document.createElement('tr');
        const username = document.createElement('td');
        username.textContent = user.username;
        row.append(username);
        for (const permission of ['upload', 'rename', 'delete']) {
          const cell = document.createElement('td');
          cell.append(permissionCheckbox(user.username, permission, user.permissions[permission]));
          row.append(cell);
        }
        const actions = document.createElement('td');
        actions.className = 'actions';
        actions.append(
          actionButton('Rename', '', async () => {
            const nextName = window.prompt('New username', user.username);
            if (!nextName || nextName === user.username) return;
            try {
              await api(`/api/console/users/${encodeURIComponent(user.username)}`, {
                method: 'PATCH', body: JSON.stringify({ username: nextName }),
              });
              await loadConsole();
              setStatus(`Renamed ${user.username} to ${nextName}.`);
            } catch (error) { setStatus(error.message, true); }
          }),
          actionButton('Reset password', '', () => {
            const dialog = document.querySelector('#reset-password-dialog');
            dialog.dataset.username = user.username;
            document.querySelector('#reset-password-value').value = '';
            dialog.showModal();
          }),
          actionButton('Delete', 'danger', async () => {
            if (!window.confirm(`Delete ${user.username} and revoke all sessions?`)) return;
            try {
              await api(`/api/console/users/${encodeURIComponent(user.username)}`, { method: 'DELETE' });
              await loadConsole();
              setStatus(`Deleted ${user.username}.`);
            } catch (error) { setStatus(error.message, true); }
          }),
        );
        row.append(actions);
        usersBody.append(row);
      }
    }

    async function loadConsole() {
      const data = await api('/api/console/users');
      renderUsers(data.users);
      for (const permission of ['upload', 'rename', 'delete']) {
        document.querySelector(`#anonymous-${permission}-permission`).checked = data.anonymousPermissions[permission];
      }
    }

    document.querySelector('#create-user-form').addEventListener('submit', async event => {
      event.preventDefault();
      const form = event.currentTarget;
      const submit = document.querySelector('#create-user-action');
      submit.disabled = true;
      try {
        const permissions = Object.fromEntries(
          ['upload', 'rename', 'delete'].map(permission => [permission, form.elements[permission].checked]),
        );
        await api('/api/console/users', {
          method: 'POST',
          body: JSON.stringify({ username: form.elements.username.value, password: form.elements.password.value, permissions }),
        });
        const username = form.elements.username.value;
        form.reset();
        await loadConsole();
        setStatus(`Created ${username}.`);
      } catch (error) { setStatus(error.message, true); }
      finally { submit.disabled = false; }
    });

    document.querySelector('#save-anonymous-permissions').addEventListener('click', async event => {
      const button = event.currentTarget;
      button.disabled = true;
      try {
        const permissions = Object.fromEntries(
          ['upload', 'rename', 'delete'].map(permission => [permission, document.querySelector(`#anonymous-${permission}-permission`).checked]),
        );
        await api('/api/console/anonymous-permissions', { method: 'PATCH', body: JSON.stringify(permissions) });
        setStatus('Updated anonymous permissions.');
      } catch (error) { setStatus(error.message, true); }
      finally { button.disabled = false; }
    });

    document.querySelector('#cancel-password-reset').addEventListener('click', () => {
      document.querySelector('#reset-password-dialog').close();
    });

    document.querySelector('#reset-password-form').addEventListener('submit', async event => {
      event.preventDefault();
      const dialog = document.querySelector('#reset-password-dialog');
      const username = dialog.dataset.username;
      const password = event.currentTarget.elements.password.value;
      try {
        await api(`/api/console/users/${encodeURIComponent(username)}/password`, {
          method: 'POST', body: JSON.stringify({ password }),
        });
        dialog.close();
        setStatus(`Reset password and revoked sessions for ${username}.`);
      } catch (error) { setStatus(error.message, true); }
    });

    loadConsole().catch(error => setStatus(error.message, true));
  </script>
</body>
</html>"#;

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>File Hub</title>
  <style>
    :root {
      color-scheme: light;
      font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #f7f8fa;
      color: #1f2328;
    }
    body {
      margin: 0;
    }
    main {
      max-width: 1080px;
      margin: 0 auto;
      padding: 32px 24px;
    }
    header {
      display: flex;
      align-items: baseline;
      justify-content: space-between;
      gap: 16px;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0;
      font-size: 28px;
      font-weight: 650;
    }
    .identity {
      font-size: 14px;
      color: #57606a;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      background: #fff;
      border: 1px solid #d8dee4;
    }
    th,
    td {
      padding: 11px 12px;
      border-bottom: 1px solid #d8dee4;
      text-align: left;
      font-size: 14px;
    }
    th {
      background: #f6f8fa;
      font-weight: 600;
    }
    tr:last-child td {
      border-bottom: 0;
    }
    .name {
      font-weight: 600;
    }
    .toolbar,
    .breadcrumb {
      display: flex;
      align-items: center;
      flex-wrap: wrap;
      gap: 8px;
      margin-bottom: 16px;
    }
    .toolbar {
      justify-content: space-between;
    }
    .filter {
      display: flex;
      align-items: center;
      gap: 8px;
    }
    label {
      font-size: 14px;
      font-weight: 600;
    }
    input,
    select {
      min-width: 260px;
      padding: 8px 10px;
      border: 1px solid #d8dee4;
      border-radius: 6px;
      font: inherit;
    }
    select {
      min-width: 180px;
    }
    button {
      padding: 0;
      border: 0;
      background: transparent;
      color: #0969da;
      font: inherit;
      cursor: pointer;
    }
    button:hover {
      text-decoration: underline;
    }
    button[hidden] {
      display: none;
    }
    .parent {
      padding: 7px 10px;
      border: 1px solid #d8dee4;
      border-radius: 6px;
      background: #fff;
    }
    .sort-button {
      color: #1f2328;
      font-weight: 600;
    }
    .sort-direction {
      margin-left: 4px;
      color: #57606a;
      font-size: 12px;
    }
    .muted {
      color: #57606a;
    }
    .error {
      color: #cf222e;
      font-weight: 600;
    }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>File Hub</h1>
      <div class="identity" aria-label="Identity Area" id="identity-area">
        <span id="identity-username">Anonymous</span>
        <button type="button" id="login-action">Login</button>
        <button type="button" id="password-change-action" hidden>Change password</button>
        <button type="button" id="logout-action" hidden>Logout</button>
        <a href="/console" id="console-entry" hidden>Console</a>
      </div>
    </header>
    <nav aria-label="Breadcrumb" class="breadcrumb" id="breadcrumb"></nav>
    <div class="toolbar">
      <button type="button" id="return-to-parent" class="parent" hidden>Return to parent</button>
      <div class="filter">
        <label for="search-mode">Search Mode</label>
        <select id="search-mode">
          <option value="currentListFilter" selected>Current List Filter</option>
          <option value="serverSearch">Server Search</option>
        </select>
        <label for="current-list-filter">Search</label>
        <input id="current-list-filter" type="search" autocomplete="off">
        <button type="button" id="server-search-submit" hidden>Search</button>
      </div>
    </div>
    <section aria-label="Root Directory">
      <table>
        <thead>
          <tr>
            <th><button type="button" class="sort-button" data-sort-field="name">Name<span class="sort-direction" data-sort-indicator="name"></span></button></th>
            <th>Kind</th>
            <th><button type="button" class="sort-button" data-sort-field="size">Size<span class="sort-direction" data-sort-indicator="size"></span></button></th>
            <th><button type="button" class="sort-button" data-sort-field="modifiedTime">Modified<span class="sort-direction" data-sort-indicator="modifiedTime"></span></button></th>
            <th>Actions</th>
          </tr>
        </thead>
        <tbody id="resources">
          <tr><td colspan="5" class="muted">Loading</td></tr>
        </tbody>
      </table>
    </section>
  </main>
  <script>
    (function () {
      var resources = document.getElementById('resources');
      var breadcrumb = document.getElementById('breadcrumb');
      var returnToParent = document.getElementById('return-to-parent');
      var searchMode = document.getElementById('search-mode');
      var filter = document.getElementById('current-list-filter');
      var serverSearchSubmit = document.getElementById('server-search-submit');
      var identityUsername = document.getElementById('identity-username');
      var loginAction = document.getElementById('login-action');
      var passwordChangeAction = document.getElementById('password-change-action');
      var logoutAction = document.getElementById('logout-action');
      var consoleEntry = document.getElementById('console-entry');
      var sortButtons = Array.prototype.slice.call(document.querySelectorAll('[data-sort-field]'));
      var sortIndicators = Array.prototype.slice.call(document.querySelectorAll('[data-sort-indicator]'));
      var state = {
        path: '',
        sort: 'name',
        order: 'asc',
        filter: '',
        searchMode: 'currentListFilter'
      };

      function text(value) {
        return document.createTextNode(value == null ? '' : String(value));
      }

      function parentPath(path) {
        var index = path.lastIndexOf('/');
        if (index === -1) {
          return '';
        }
        return path.slice(0, index);
      }

      function loadDirectory(path) {
        state.path = path || '';
        var query = [
          'path=' + encodeURIComponent(state.path),
          'sort=' + encodeURIComponent(state.sort),
          'order=' + encodeURIComponent(state.order),
          'filter=' + encodeURIComponent(state.filter)
        ].join('&');

        fetch('/api/list' + (query ? '?' + query : ''))
          .then(function (response) {
            return response.json().then(function (body) {
              if (!response.ok) {
                throw new Error(body.error && body.error.reason ? body.error.reason : 'Listing failed');
              }
              return body;
            });
          })
          .then(function (body) {
            state.path = body.path || '';
            renderBreadcrumb(body.breadcrumbs || []);
            renderParentAction();
            renderSearchMode();
            renderSort(body.sort || { field: state.sort, order: state.order });
            renderRows(body.resources || []);
          })
          .catch(function (error) {
            renderError(error.message);
          });
      }

      function loadIdentity() {
        fetch('/api/identity')
          .then(function (response) {
            if (!response.ok) {
              throw new Error('Identity load failed');
            }
            return response.json();
          })
          .then(renderIdentity)
          .catch(function () {
            renderIdentity({
              authenticated: false,
              actions: { login: true, passwordChange: false, logout: false, console: false }
            });
          });
      }

      function renderIdentity(identity) {
        identityUsername.textContent = identity.authenticated ? identity.username : 'Anonymous';
        loginAction.hidden = !identity.actions.login;
        passwordChangeAction.hidden = !identity.actions.passwordChange;
        logoutAction.hidden = !identity.actions.logout;
        consoleEntry.hidden = !identity.actions.console;
      }

      function login() {
        var username = window.prompt('Username');
        if (!username) {
          return;
        }
        var password = window.prompt('Password');
        if (!password) {
          return;
        }
        fetch('/api/login', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ username: username, password: password })
        }).then(loadIdentity);
      }

      function changePassword() {
        var oldPassword = window.prompt('Current password');
        if (!oldPassword) {
          return;
        }
        var newPassword = window.prompt('New password');
        if (!newPassword) {
          return;
        }
        fetch('/api/password', {
          method: 'POST',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify({ oldPassword: oldPassword, newPassword: newPassword })
        }).then(loadIdentity);
      }

      function logout() {
        fetch('/api/logout', { method: 'POST' }).then(loadIdentity);
      }

      function renderBreadcrumb(segments) {
        breadcrumb.textContent = '';
        segments.forEach(function (segment, index) {
          if (index > 0) {
            breadcrumb.appendChild(text('/'));
          }
          var button = document.createElement('button');
          button.type = 'button';
          button.appendChild(text(segment.label));
          button.addEventListener('click', function () {
            loadDirectory(segment.path);
          });
          breadcrumb.appendChild(button);
        });
      }

      function renderParentAction() {
        if (!state.path) {
          returnToParent.hidden = true;
          return;
        }

        returnToParent.hidden = false;
      }

      function downloadResource(resourcePath) {
        window.location.assign('/api/download?path=' + encodeURIComponent(resourcePath));
      }

      function downloadDirectoryArchive(resourcePath) {
        window.location.assign('/api/archive?path=' + encodeURIComponent(resourcePath));
      }

      function renderSearchMode() {
        serverSearchSubmit.hidden = state.searchMode !== 'serverSearch';
      }

      function runServerSearch() {
        fetch('/api/search?q=' + encodeURIComponent(state.filter))
          .then(function (response) {
            return response.json().then(function (body) {
              if (!response.ok) {
                throw new Error(body.error && body.error.reason ? body.error.reason : 'Search failed');
              }
              return body;
            });
          })
          .then(function (body) {
            renderSearchRows(body.resources || [], Boolean(body.truncated));
          })
          .catch(function (error) {
            renderError(error.message);
          });
      }

      function renderSort(sort) {
        state.sort = sort.field || state.sort;
        state.order = sort.order || state.order;
        sortIndicators.forEach(function (indicator) {
          var field = indicator.getAttribute('data-sort-indicator');
          indicator.textContent = field === state.sort ? state.order : '';
        });
      }

      function renderSearchRows(rows, truncated) {
        resources.textContent = '';
        if (truncated) {
          var truncatedRow = document.createElement('tr');
          var truncatedCell = document.createElement('td');
          truncatedCell.colSpan = 5;
          truncatedCell.className = 'muted';
          truncatedCell.appendChild(text('Search results truncated'));
          truncatedRow.appendChild(truncatedCell);
          resources.appendChild(truncatedRow);
        }
        if (!rows.length) {
          var emptyRow = document.createElement('tr');
          var emptyCell = document.createElement('td');
          emptyCell.colSpan = 5;
          emptyCell.className = 'muted';
          emptyCell.appendChild(text('Empty'));
          emptyRow.appendChild(emptyCell);
          resources.appendChild(emptyRow);
          return;
        }

        rows.forEach(function (row) {
          var resource = row.resource;
          var resultRow = document.createElement('tr');
          var name = document.createElement('td');
          name.className = 'name';
          if (resource.kind === 'directory') {
            var open = document.createElement('button');
            open.type = 'button';
            open.appendChild(text(resource.name));
            open.addEventListener('click', function () {
              loadDirectory(row.resource.resourcePath);
            });
            name.appendChild(open);
          } else {
            var download = document.createElement('button');
            download.type = 'button';
            download.appendChild(text(resource.name));
            download.addEventListener('click', function () {
              downloadResource(row.resource.resourcePath);
            });
            name.appendChild(download);
          }
          var kind = document.createElement('td');
          kind.appendChild(text(resource.kind));
          var size = document.createElement('td');
          size.appendChild(text(resource.size));
          var containingPath = document.createElement('td');
          var containingPathButton = document.createElement('button');
          containingPathButton.type = 'button';
          containingPathButton.appendChild(text(row.containingPath || 'Root Directory'));
          containingPathButton.addEventListener('click', function () {
            loadDirectory(row.containingPath);
          });
          containingPath.appendChild(containingPathButton);
          var actions = document.createElement('td');
          if (resource.kind === 'directory') {
            var archive = document.createElement('button');
            archive.type = 'button';
            archive.setAttribute('aria-label', 'Download archive for ' + resource.name);
            archive.appendChild(text('Download archive'));
            archive.addEventListener('click', function () {
              downloadDirectoryArchive(row.resource.resourcePath);
            });
            actions.appendChild(archive);
          }
          resultRow.appendChild(name);
          resultRow.appendChild(kind);
          resultRow.appendChild(size);
          resultRow.appendChild(containingPath);
          resultRow.appendChild(actions);
          resources.appendChild(resultRow);
        });
      }

      function renderRows(rows) {
        resources.textContent = '';
        if (!rows.length) {
          var emptyRow = document.createElement('tr');
          var emptyCell = document.createElement('td');
          emptyCell.colSpan = 5;
          emptyCell.className = 'muted';
          emptyCell.appendChild(text('Empty'));
          emptyRow.appendChild(emptyCell);
          resources.appendChild(emptyRow);
          return;
        }

        rows.forEach(function (resource) {
          var row = document.createElement('tr');
          var name = document.createElement('td');
          name.className = 'name';
          if (resource.kind === 'directory') {
            var open = document.createElement('button');
            open.type = 'button';
            open.appendChild(text(resource.name));
            open.addEventListener('click', function () {
              loadDirectory(resource.resourcePath);
            });
            name.appendChild(open);
          } else {
            var download = document.createElement('button');
            download.type = 'button';
            download.appendChild(text(resource.name));
            download.addEventListener('click', function () {
              downloadResource(resource.resourcePath);
            });
            name.appendChild(download);
          }
          var kind = document.createElement('td');
          kind.appendChild(text(resource.kind));
          var size = document.createElement('td');
          size.appendChild(text(resource.size));
          var modified = document.createElement('td');
          modified.appendChild(text(resource.modifiedTime));
          var actions = document.createElement('td');
          if (resource.kind === 'directory') {
            var archive = document.createElement('button');
            archive.type = 'button';
            archive.setAttribute('aria-label', 'Download archive for ' + resource.name);
            archive.appendChild(text('Download archive'));
            archive.addEventListener('click', function () {
              downloadDirectoryArchive(resource.resourcePath);
            });
            actions.appendChild(archive);
          }
          row.appendChild(name);
          row.appendChild(kind);
          row.appendChild(size);
          row.appendChild(modified);
          row.appendChild(actions);
          resources.appendChild(row);
        });
      }

      function renderError(message) {
        resources.textContent = '';
        var row = document.createElement('tr');
        var cell = document.createElement('td');
        cell.colSpan = 5;
        cell.className = 'error';
        cell.appendChild(text(message));
        row.appendChild(cell);
        resources.appendChild(row);
      }

      returnToParent.addEventListener('click', function () {
        loadDirectory(parentPath(state.path));
      });
      loginAction.addEventListener('click', login);
      passwordChangeAction.addEventListener('click', changePassword);
      logoutAction.addEventListener('click', logout);

      searchMode.addEventListener('change', function () {
        state.searchMode = searchMode.value;
        renderSearchMode();
        if (state.searchMode === 'currentListFilter') {
          loadDirectory(state.path);
        }
      });

      filter.addEventListener('input', function () {
        state.filter = filter.value;
        if (state.searchMode === 'currentListFilter') {
          loadDirectory(state.path);
        }
      });

      serverSearchSubmit.addEventListener('click', function () {
        runServerSearch();
      });

      sortButtons.forEach(function (button) {
        button.addEventListener('click', function () {
          var field = button.getAttribute('data-sort-field');
          if (state.sort === field) {
            state.order = state.order === 'asc' ? 'desc' : 'asc';
          } else {
            state.sort = field;
            state.order = 'asc';
          }
          loadDirectory(state.path);
        });
      });

      loadIdentity();
      loadDirectory('');
    }());
  </script>
</body>
</html>
"#;
