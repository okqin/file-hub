//! HTTP routes for File Hub.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, FromRequest, FromRequestParts, Multipart, Path as AxumPath, Query, State,
        multipart::Field,
    },
    http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header, request::Parts},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::io::ReaderStream;
use tower::ServiceBuilder;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};
use tracing::warn;

use crate::{
    auth::{AuthError, AuthState, BootstrapPassword, BootstrapReport, ManagedUser, PermissionSet},
    config::AppConfig,
    resource,
    resource_address::MAX_RESOURCE_PATH_BYTES,
    upload,
};

const SESSION_COOKIE_NAME: &str = "fh_session";

#[derive(Debug, RustEmbed)]
#[folder = "frontend-dist/"]
struct FrontendAssets;

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
    upload::cleanup_staging_remnants(&config).await?;
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
    let request_body_limit =
        usize::try_from(config.limits().request_body_limit_bytes().get()).unwrap_or(usize::MAX);
    let request_semaphore = Arc::new(Semaphore::new(
        config.limits().request_concurrency_limit().get(),
    ));
    let fs_semaphore = Arc::new(Semaphore::new(config.limits().fs_concurrency_limit().get()));
    let state = AppState {
        config: Arc::new(config),
        auth,
    };

    let resource_router = Router::new()
        .route("/api/list", get(list_root_directory))
        .route("/api/search", get(search_resources))
        .route("/api/download", get(download_file))
        .route("/api/archive", get(download_directory_archive))
        .route("/api/mkdir", post(create_directory))
        .route("/api/rename", post(rename_resource))
        .route("/api/delete", post(delete_resource))
        .route("/api/upload", post(upload_file))
        .layer(middleware::from_fn_with_state(
            fs_semaphore,
            enforce_fs_concurrency_limit,
        ));

    let router = Router::new()
        .route("/", get(spa_index))
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
        .merge(resource_router)
        .fallback(spa_fallback)
        .with_state(state)
        .layer(DefaultBodyLimit::max(request_body_limit))
        .layer(middleware::from_fn_with_state(
            request_semaphore,
            enforce_request_concurrency_limit,
        ))
        .layer(ServiceBuilder::new().layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        )))
        .layer(middleware::map_response(normalize_framework_error))
        .layer(TraceLayer::new_for_http());

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
    /// Reserved staging remnants could not be cleaned safely.
    #[error("staging cleanup failed")]
    StagingCleanup(#[from] upload::UploadError),
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
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CreateDirectoryRequest {
    path: String,
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RenameResourceRequest {
    path: String,
    new_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DeleteResourceRequest {
    path: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    reason: String,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    path: Option<String>,
    reason: String,
}

async fn spa_index() -> Result<Response, ApiError> {
    embedded_frontend_asset("index.html", false)
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

async fn console_view(_administrator: Administrator) -> Result<Response, ApiError> {
    spa_index().await
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

async fn create_directory(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<StatusCode, ApiError> {
    require_upload_permission(&state, request.headers()).await?;
    let Json(request) = Json::<CreateDirectoryRequest>::from_request(request, &state)
        .await
        .map_err(|_| ApiError::invalid_create_directory_request())?;
    resource::create_directory(&state.config, &request.path, &request.name).await?;
    Ok(StatusCode::CREATED)
}

async fn rename_resource(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<StatusCode, ApiError> {
    require_rename_permission(&state, request.headers()).await?;
    let Json(request) = Json::<RenameResourceRequest>::from_request(request, &state)
        .await
        .map_err(|_| ApiError::invalid_rename_request())?;
    resource::rename_resource(&state.config, &request.path, &request.new_name)
        .await
        .map_err(|error| ApiError::from_rename_error(error, &request.path))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_resource(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<StatusCode, ApiError> {
    require_delete_permission(&state, request.headers()).await?;
    let Json(request) = Json::<DeleteResourceRequest>::from_request(request, &state)
        .await
        .map_err(|_| ApiError::invalid_delete_request())?;
    resource::delete_resource(&state.config, &request.path)
        .await
        .map_err(|error| ApiError::from_delete_error(error, &request.path))?;
    Ok(StatusCode::NO_CONTENT)
}

async fn upload_file(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> Result<StatusCode, ApiError> {
    require_upload_permission(&state, request.headers()).await?;
    let multipart = Multipart::from_request(request, &state)
        .await
        .map_err(|_| ApiError::invalid_upload_request())?;
    upload::execute(&state.config, MultipartUploadInput { multipart }).await?;
    Ok(StatusCode::CREATED)
}

#[derive(Debug)]
struct MultipartUploadInput {
    multipart: Multipart,
}

impl upload::UploadInput for MultipartUploadInput {
    async fn consume(
        mut self,
        receiver: &mut upload::UploadReceiver<'_>,
    ) -> Result<(), upload::UploadError> {
        let mut path_field = next_upload_field(&mut self.multipart).await?;
        if path_field.name() != Some("path") {
            return Err(upload::UploadError::InvalidInput);
        }
        let current_path = read_upload_path(&mut path_field).await?;
        drop(path_field);
        let mut field = next_upload_field(&mut self.multipart).await?;

        match field.name() {
            Some("file") => {
                let filename = field
                    .file_name()
                    .map(str::to_owned)
                    .ok_or(upload::UploadError::InvalidInput)?;
                receiver
                    .begin(&current_path, upload::UploadKind::File)
                    .await?;
                receiver.receive_file(&filename, field).await?;
                if next_optional_upload_field(&mut self.multipart)
                    .await?
                    .is_some()
                {
                    return Err(upload::UploadError::InvalidInput);
                }
            }
            Some("relativePath") => {
                receiver
                    .begin(&current_path, upload::UploadKind::Directory)
                    .await?;
                loop {
                    let relative_path = read_upload_path(&mut field).await?;
                    drop(field);
                    let file = next_upload_field(&mut self.multipart).await?;
                    if file.name() != Some("file") {
                        return Err(upload::UploadError::InvalidInput);
                    }
                    receiver.receive_file(&relative_path, file).await?;
                    let Some(next_field) = next_optional_upload_field(&mut self.multipart).await?
                    else {
                        break;
                    };
                    if next_field.name() != Some("relativePath") {
                        return Err(upload::UploadError::InvalidInput);
                    }
                    field = next_field;
                }
            }
            _ => return Err(upload::UploadError::InvalidInput),
        }
        Ok(())
    }
}

async fn next_upload_field(multipart: &mut Multipart) -> Result<Field<'_>, upload::UploadError> {
    next_optional_upload_field(multipart)
        .await?
        .ok_or(upload::UploadError::InvalidInput)
}

async fn next_optional_upload_field(
    multipart: &mut Multipart,
) -> Result<Option<Field<'_>>, upload::UploadError> {
    multipart
        .next_field()
        .await
        .map_err(|error| upload::UploadError::InputRead(Box::new(error)))
}

async fn read_upload_path(field: &mut Field<'_>) -> Result<String, upload::UploadError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = field
        .chunk()
        .await
        .map_err(|error| upload::UploadError::InputRead(Box::new(error)))?
    {
        let next_length = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(upload::UploadError::InvalidInput)?;
        if next_length > MAX_RESOURCE_PATH_BYTES {
            return Err(upload::UploadError::InvalidInput);
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| upload::UploadError::InvalidInput)
}

async fn require_upload_permission(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let permissions = effective_permissions(state, headers).await?;
    if permissions.upload() {
        Ok(())
    } else {
        Err(ApiError::upload_permission_required())
    }
}

async fn require_rename_permission(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let permissions = effective_permissions(state, headers).await?;
    if permissions.rename() {
        Ok(())
    } else {
        Err(ApiError::rename_permission_required())
    }
}

async fn require_delete_permission(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let permissions = effective_permissions(state, headers).await?;
    if permissions.delete() {
        Ok(())
    } else {
        Err(ApiError::delete_permission_required())
    }
}

async fn effective_permissions(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<PermissionSet, ApiError> {
    let token = session_token_from_headers(headers);
    Ok(
        match state.auth.identity_for_session(token.as_deref()).await? {
            Some(identity) => identity.permissions(),
            None => state.auth.anonymous_permissions().await?,
        },
    )
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
                path: None,
                reason: "resource path is invalid".to_owned(),
            },
            resource::ResourceError::InvalidFilter => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_filter",
                path: None,
                reason: "current list filter query is invalid".to_owned(),
            },
            resource::ResourceError::InvalidSearchQuery => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_search_query",
                path: None,
                reason: "server search query is invalid".to_owned(),
            },
            resource::ResourceError::NotDirectory => Self {
                status: StatusCode::BAD_REQUEST,
                code: "not_directory",
                path: None,
                reason: "resource path is not a directory".to_owned(),
            },
            resource::ResourceError::NotFile => Self {
                status: StatusCode::BAD_REQUEST,
                code: "not_file",
                path: None,
                reason: "resource path is not a file".to_owned(),
            },
            error @ (resource::ResourceError::RootDirectoryArchive
            | resource::ResourceError::RootDirectoryRename
            | resource::ResourceError::RootDirectoryDelete) => {
                Self::from_root_directory_error(&error)
            }
            resource::ResourceError::ResourceNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "resource_not_found",
                path: None,
                reason: "resource path does not exist".to_owned(),
            },
            error @ (resource::ResourceError::ListingLimitExceeded { .. }
            | resource::ResourceError::ArchiveResourceCountLimitExceeded { .. }
            | resource::ResourceError::ArchiveSizeLimitExceeded { .. }) => {
                Self::from_resource_limit_error(&error)
            }
            resource::ResourceError::InvalidResourceName => Self::invalid_resource_name(),
            error @ (resource::ResourceError::InvalidWriteResourceName
            | resource::ResourceError::NameConflict
            | resource::ResourceError::CreateDirectory(_)
            | resource::ResourceError::RenameResource(_)) => Self::from_write_error(&error),
            resource::ResourceError::DeleteResource {
                path,
                operation,
                source,
            } => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "delete_failed",
                path: Some(path),
                reason: format!("failed to {operation}: {source}"),
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
                path: None,
                reason: "failed to read Resource".to_owned(),
            },
        }
    }
}

impl From<upload::UploadError> for ApiError {
    fn from(error: upload::UploadError) -> Self {
        match error {
            upload::UploadError::InvalidInput | upload::UploadError::InputRead(_) => {
                Self::invalid_upload_request()
            }
            upload::UploadError::InvalidResourcePath => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_resource_path",
                path: None,
                reason: "resource path is invalid".to_owned(),
            },
            upload::UploadError::NotDirectory => Self {
                status: StatusCode::BAD_REQUEST,
                code: "not_directory",
                path: None,
                reason: "resource path is not a directory".to_owned(),
            },
            upload::UploadError::ResourceNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "resource_not_found",
                path: None,
                reason: "resource path does not exist".to_owned(),
            },
            upload::UploadError::InvalidWriteResourceName => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_resource_name",
                path: None,
                reason: "Resource Name is invalid".to_owned(),
            },
            upload::UploadError::InvalidDirectoryUploadPath { path } => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_directory_upload_path",
                path: Some(path),
                reason: "relative path contains an invalid Resource Name".to_owned(),
            },
            upload::UploadError::DirectoryUploadConflict { path } => Self {
                status: StatusCode::CONFLICT,
                code: "directory_upload_conflict",
                path: Some(path),
                reason: "Directory Upload conflicts with an existing Resource".to_owned(),
            },
            upload::UploadError::DirectoryUploadSingleFileSizeLimitExceeded { path, limit } => {
                Self {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "directory_upload_single_file_size_limit_exceeded",
                    path: Some(path),
                    reason: format!(
                        "File exceeds the configured Directory Upload single-file limit of \
                         {limit} bytes"
                    ),
                }
            }
            upload::UploadError::DirectoryUploadTotalSizeLimitExceeded { path, limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "directory_upload_total_size_limit_exceeded",
                path: Some(path),
                reason: format!(
                    "File causes the Directory Upload total size limit of {limit} bytes to be \
                     exceeded"
                ),
            },
            upload::UploadError::DirectoryUploadResourceCountLimitExceeded { path, limit } => {
                Self {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "directory_upload_resource_count_limit_exceeded",
                    path: Some(path),
                    reason: format!(
                        "Resource causes the Directory Upload count limit of {limit} to be \
                         exceeded"
                    ),
                }
            }
            upload::UploadError::NameConflict => Self {
                status: StatusCode::CONFLICT,
                code: "name_conflict",
                path: None,
                reason: "Resource Name conflicts with an existing Resource".to_owned(),
            },
            upload::UploadError::UploadSingleFileSizeLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "upload_single_file_size_limit_exceeded",
                path: None,
                reason: format!("uploaded File exceeds configured size limit of {limit} bytes"),
            },
            upload::UploadError::UploadTotalSizeLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "upload_total_size_limit_exceeded",
                path: None,
                reason: format!("upload exceeds configured total size limit of {limit} bytes"),
            },
            upload::UploadError::Store(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "upload_failed",
                path: None,
                reason: "failed to store uploaded File".to_owned(),
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
                path: None,
                reason: "username is invalid".to_owned(),
            },
            AuthError::InvalidPassword => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_password",
                path: None,
                reason: "password is invalid".to_owned(),
            },
            AuthError::ReservedAdministrator => Self::reserved_administrator(),
            AuthError::UsernameConflict => Self {
                status: StatusCode::CONFLICT,
                code: "username_conflict",
                path: None,
                reason: "username already exists".to_owned(),
            },
            AuthError::UserNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "user_not_found",
                path: None,
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
    fn from_resource_limit_error(error: &resource::ResourceError) -> Self {
        let (code, reason) = match error {
            resource::ResourceError::ListingLimitExceeded { limit } => (
                "listing_limit_exceeded",
                format!("direct child listing exceeds configured limit of {limit}"),
            ),
            resource::ResourceError::ArchiveResourceCountLimitExceeded { limit } => (
                "archive_resource_count_limit_exceeded",
                format!("directory archive exceeds configured resource count limit of {limit}"),
            ),
            resource::ResourceError::ArchiveSizeLimitExceeded { limit } => (
                "archive_size_limit_exceeded",
                format!(
                    "directory archive exceeds configured uncompressed size limit of {limit} bytes"
                ),
            ),
            _ => return Self::internal_server_error(),
        };
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code,
            path: None,
            reason,
        }
    }

    fn from_root_directory_error(error: &resource::ResourceError) -> Self {
        let (code, reason) = match error {
            resource::ResourceError::RootDirectoryArchive => (
                "root_directory_archive",
                "Root Directory cannot be downloaded as an archive",
            ),
            resource::ResourceError::RootDirectoryRename => {
                ("root_directory_rename", "Root Directory cannot be renamed")
            }
            resource::ResourceError::RootDirectoryDelete => {
                ("root_directory_delete", "Root Directory cannot be deleted")
            }
            _ => return Self::internal_server_error(),
        };
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            path: None,
            reason: reason.to_owned(),
        }
    }

    fn from_rename_error(error: resource::ResourceError, path: &str) -> Self {
        let mut error = Self::from(error);
        error.path = Some(path.to_owned());
        error
    }

    fn from_delete_error(error: resource::ResourceError, path: &str) -> Self {
        let mut error = Self::from(error);
        if error.path.is_none() {
            error.path = Some(path.to_owned());
        }
        error
    }

    fn invalid_resource_name() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "invalid_resource_name",
            path: None,
            reason: "resource name is not valid UTF-8".to_owned(),
        }
    }

    fn from_write_error(error: &resource::ResourceError) -> Self {
        match error {
            resource::ResourceError::InvalidWriteResourceName => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_resource_name",
                path: None,
                reason: "Resource Name is invalid".to_owned(),
            },
            resource::ResourceError::NameConflict => Self {
                status: StatusCode::CONFLICT,
                code: "name_conflict",
                path: None,
                reason: "Resource Name conflicts with an existing Resource".to_owned(),
            },
            resource::ResourceError::CreateDirectory(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "create_directory_failed",
                path: None,
                reason: "failed to create Directory".to_owned(),
            },
            resource::ResourceError::RenameResource(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "rename_failed",
                path: None,
                reason: "failed to rename Resource".to_owned(),
            },
            _ => Self::internal_server_error(),
        }
    }

    fn internal_server_error() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_server_error",
            path: None,
            reason: "internal server error".to_owned(),
        }
    }

    fn invalid_credentials() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "invalid_credentials",
            path: None,
            reason: "username or password is invalid".to_owned(),
        }
    }

    fn authentication_required() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "authentication_required",
            path: None,
            reason: "authentication is required".to_owned(),
        }
    }

    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "forbidden",
            path: None,
            reason: "administrator identity is required".to_owned(),
        }
    }

    fn upload_permission_required() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "upload_permission_required",
            path: None,
            reason: "Upload Permission is required".to_owned(),
        }
    }

    fn rename_permission_required() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "rename_permission_required",
            path: None,
            reason: "Rename Permission is required".to_owned(),
        }
    }

    fn delete_permission_required() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "delete_permission_required",
            path: None,
            reason: "Delete Permission is required".to_owned(),
        }
    }

    fn invalid_rename_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_rename_request",
            path: None,
            reason: "Rename request is invalid".to_owned(),
        }
    }

    fn invalid_delete_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_delete_request",
            path: None,
            reason: "Delete request is invalid".to_owned(),
        }
    }

    fn invalid_upload_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_upload_request",
            path: None,
            reason: "upload request is invalid".to_owned(),
        }
    }

    fn request_body_too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "request_body_too_large",
            path: None,
            reason: "request body exceeds configured limit".to_owned(),
        }
    }

    fn request_concurrency_limit_exceeded() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "request_concurrency_limit_exceeded",
            path: None,
            reason: "request concurrency limit exceeded".to_owned(),
        }
    }

    fn request_timeout() -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "request_timeout",
            path: None,
            reason: "request exceeded configured timeout".to_owned(),
        }
    }

    fn fs_concurrency_limit_exceeded() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "fs_concurrency_limit_exceeded",
            path: None,
            reason: "filesystem concurrency limit exceeded".to_owned(),
        }
    }

    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code: "not_found",
            path: None,
            reason: "resource not found".to_owned(),
        }
    }

    fn invalid_create_directory_request() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_create_directory_request",
            path: None,
            reason: "create Directory request is invalid".to_owned(),
        }
    }

    fn reserved_administrator() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "reserved_administrator",
            path: None,
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
                    path: self.path,
                    reason: self.reason,
                },
            }),
        )
            .into_response()
    }
}

async fn normalize_framework_error(response: Response) -> Response {
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE
        && response.headers().get(header::CONTENT_TYPE)
            != Some(&HeaderValue::from_static("application/json"))
    {
        return ApiError::request_body_too_large().into_response();
    }
    if response.status() == StatusCode::REQUEST_TIMEOUT {
        return ApiError::request_timeout().into_response();
    }
    response
}

async fn spa_fallback(uri: Uri) -> Result<Response, ApiError> {
    let path = uri.path().trim_start_matches('/');
    if uri.path() == "/api" || uri.path().starts_with("/api/") {
        return Err(ApiError::not_found());
    }
    if path.starts_with("assets/") {
        return embedded_frontend_asset(path, true);
    }
    embedded_frontend_asset("index.html", false)
}

fn embedded_frontend_asset(path: &str, immutable: bool) -> Result<Response, ApiError> {
    let asset = FrontendAssets::get(path).ok_or_else(ApiError::not_found)?;
    let content_type = match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("css") => "text/css; charset=utf-8",
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    let cache_control = if immutable {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(asset.data.into_owned()))
        .map_err(|_| ApiError::internal_server_error())
}

async fn enforce_request_concurrency_limit(
    State(semaphore): State<Arc<Semaphore>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(_permit) = semaphore.try_acquire_owned() else {
        return ApiError::request_concurrency_limit_exceeded().into_response();
    };
    next.run(request).await
}

async fn enforce_fs_concurrency_limit(
    State(semaphore): State<Arc<Semaphore>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(_permit) = semaphore.try_acquire_owned() else {
        return ApiError::fs_concurrency_limit_exceeded().into_response();
    };
    next.run(request).await
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
