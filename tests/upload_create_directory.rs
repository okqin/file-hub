use std::{convert::Infallible, path::Path, time::Duration};

use anyhow::{Context, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use file_hub::{
    auth::{AuthState, PermissionSet},
    config::AppConfig,
    http::build_router,
};
use futures_util::stream;
use tempfile::TempDir;
use tokio::fs;
use tower::ServiceExt;

#[tokio::test]
async fn test_should_create_directory_in_current_resource_path_with_upload_permission() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("documents"))
        .await
        .context("create current resource path")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;

    let response = app
        .clone()
        .oneshot(json_request(
            "/api/mkdir",
            &serde_json::json!({ "path": "documents", "name": "reports" }),
        )?)
        .await
        .context("send create Directory request")?;

    assert_eq!(response.status(), StatusCode::CREATED);
    let listing = app
        .oneshot(
            Request::builder()
                .uri("/api/list?path=documents")
                .body(Body::empty())
                .context("build listing request")?,
        )
        .await
        .context("list current Resource Path")?;
    assert_eq!(listing.status(), StatusCode::OK);
    let body = to_bytes(listing.into_body(), usize::MAX)
        .await
        .context("read listing response")?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).context("decode listing response")?;
    assert_eq!(body.pointer("/resources/0/name"), Some(&"reports".into()));
    assert_eq!(body.pointer("/resources/0/kind"), Some(&"directory".into()));
    Ok(())
}

#[tokio::test]
async fn test_should_deny_direct_create_requests_without_upload_permission() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let mkdir = app
        .clone()
        .oneshot(json_request(
            "/api/mkdir",
            &serde_json::json!({ "path": "", "name": "forbidden" }),
        )?)
        .await
        .context("send direct create Directory request")?;
    assert_error(mkdir, StatusCode::FORBIDDEN, "upload_permission_required").await?;

    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(Body::from("forbidden"))
                .context("build direct upload request")?,
        )
        .await
        .context("send direct upload request")?;
    assert_error(upload, StatusCode::FORBIDDEN, "upload_permission_required").await?;

    let malformed_mkdir = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/mkdir")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not-json"))
                .context("build malformed create Directory request")?,
        )
        .await
        .context("send malformed create Directory request")?;
    assert_error(
        malformed_mkdir,
        StatusCode::FORBIDDEN,
        "upload_permission_required",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn test_should_upload_file_atomically_into_current_resource_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("documents"))
        .await
        .context("create current resource path")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;

    let (content_type, body) = multipart_file("documents", "report.txt", b"complete report");
    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .context("build upload request")?,
        )
        .await
        .context("upload File")?;
    assert_eq!(upload.status(), StatusCode::CREATED);

    let download = app
        .oneshot(
            Request::builder()
                .uri("/api/download?path=documents%2Freport.txt")
                .body(Body::empty())
                .context("build download request")?,
        )
        .await
        .context("download uploaded File")?;
    assert_eq!(download.status(), StatusCode::OK);
    let body = to_bytes(download.into_body(), usize::MAX)
        .await
        .context("read uploaded File")?;
    assert_eq!(body.as_ref(), b"complete report");
    Ok(())
}

#[tokio::test]
async fn test_should_reject_multiple_top_level_files_in_one_upload_request() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let (content_type, body) = multipart_two_files();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .context("build multiple File upload request")?,
        )
        .await
        .context("send multiple File upload request")?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_upload_request").await?;
    assert!(!storage_root.path().join("first.txt").exists());
    assert!(!storage_root.path().join("second.txt").exists());
    wait_for_staging_empty(storage_root.path()).await
}

#[tokio::test]
async fn test_should_map_invalid_resource_name_for_create_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;
    let response = app
        .oneshot(json_request(
            "/api/mkdir",
            &serde_json::json!({ "path": "", "name": "nested/name" }),
        )?)
        .await
        .context("send invalid Resource Name")?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_name").await
}

#[tokio::test]
async fn test_should_reject_name_conflicts_without_overwriting_or_renaming() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("existing-directory"))
        .await
        .context("create existing Directory")?;
    fs::write(storage_root.path().join("existing.txt"), b"original")
        .await
        .context("write existing File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;

    let mkdir = app
        .clone()
        .oneshot(json_request(
            "/api/mkdir",
            &serde_json::json!({ "path": "", "name": "existing-directory" }),
        )?)
        .await
        .context("send conflicting create Directory request")?;
    assert_error(mkdir, StatusCode::CONFLICT, "name_conflict").await?;

    let (content_type, body) = multipart_file("", "existing.txt", b"replacement");
    let upload = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .context("build conflicting upload request")?,
        )
        .await
        .context("send conflicting upload request")?;
    assert_error(upload, StatusCode::CONFLICT, "name_conflict").await?;

    let download = app
        .oneshot(
            Request::builder()
                .uri("/api/download?path=existing.txt")
                .body(Body::empty())
                .context("build existing File download request")?,
        )
        .await
        .context("download existing File")?;
    let content = to_bytes(download.into_body(), usize::MAX)
        .await
        .context("read existing File")?;
    assert_eq!(content.as_ref(), b"original");
    assert!(!storage_root.path().join("existing (1).txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_enforce_single_file_and_total_upload_limits() -> Result<()> {
    let single_root = tempfile::tempdir().context("create single limit storage root")?;
    let (single_app, single_config, _single_config_dir) =
        app_from_storage_root_with_upload_limits(single_root.path(), 3, 10).await?;
    grant_anonymous_upload_permission(&single_config).await?;
    let single_response = upload_request(single_app, "single.txt", b"four").await?;
    assert_error(
        single_response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "upload_single_file_size_limit_exceeded",
    )
    .await?;
    assert!(!single_root.path().join("single.txt").exists());

    let total_root = tempfile::tempdir().context("create total limit storage root")?;
    let (total_app, total_config, _total_config_dir) =
        app_from_storage_root_with_upload_limits(total_root.path(), 10, 3).await?;
    grant_anonymous_upload_permission(&total_config).await?;
    let total_response = upload_request(total_app, "total.txt", b"four").await?;
    assert_error(
        total_response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "upload_total_size_limit_exceeded",
    )
    .await?;
    assert!(!total_root.path().join("total.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_map_invalid_resource_name_for_file_upload() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let response = upload_request(app, "nested/name", b"content").await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn test_should_reject_oversized_upload_path_without_staging() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let (content_type, body) = multipart_file(&"x".repeat(4097), "safe.txt", b"content");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .context("build oversized Resource Path upload")?,
        )
        .await
        .context("send oversized Resource Path upload")?;
    assert_error(response, StatusCode::BAD_REQUEST, "invalid_upload_request").await?;
    assert!(!storage_root.path().join(".fh-staging").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_hide_partial_file_and_reserved_staging_during_upload() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let (content_type, first, second) = delayed_multipart_file("atomic.txt");
    let chunks = vec![
        (Duration::ZERO, first),
        (Duration::from_millis(250), second),
    ];
    let body_stream = stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let (delay, chunk) = chunks.next()?;
        tokio::time::sleep(delay).await;
        Some((Ok::<_, Infallible>(chunk), chunks))
    });
    let upload_app = app.clone();
    let upload = tokio::spawn(async move {
        upload_app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/upload")
                    .header(header::CONTENT_TYPE, content_type)
                    .body(Body::from_stream(body_stream))
                    .context("build delayed upload request")?,
            )
            .await
            .context("send delayed upload request")
    });

    wait_for_staged_file(storage_root.path()).await?;
    let listing = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/list")
                .body(Body::empty())
                .context("build in-flight listing request")?,
        )
        .await
        .context("list during upload")?;
    let listing_body = to_bytes(listing.into_body(), usize::MAX)
        .await
        .context("read in-flight listing")?;
    let listing_body: serde_json::Value =
        serde_json::from_slice(&listing_body).context("decode in-flight listing")?;
    assert_eq!(listing_body.pointer("/resources/0"), None);

    let partial_download = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/download?path=atomic.txt")
                .body(Body::empty())
                .context("build partial download request")?,
        )
        .await
        .context("download during upload")?;
    assert_error(
        partial_download,
        StatusCode::NOT_FOUND,
        "resource_not_found",
    )
    .await?;

    let upload = upload.await.context("join delayed upload task")??;
    assert_eq!(upload.status(), StatusCode::CREATED);
    let completed = fs::read(storage_root.path().join("atomic.txt"))
        .await
        .context("read completed upload")?;
    assert_eq!(completed, b"partial-complete");
    Ok(())
}

#[tokio::test]
async fn test_should_enforce_authenticated_users_own_upload_permission() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;
    auth.create_user("uploader", "upload-password")
        .await
        .context("create User without Upload Permission")?;
    let session = auth
        .login("uploader", "upload-password")
        .await
        .context("log in User")?
        .context("credentials should create session")?;
    let cookie = format!("fh_session={}", session.token());

    let denied = app
        .clone()
        .oneshot(json_request_with_cookie(
            "/api/mkdir",
            &serde_json::json!({ "path": "", "name": "denied" }),
            &cookie,
        )?)
        .await
        .context("send denied authenticated create request")?;
    assert_error(denied, StatusCode::FORBIDDEN, "upload_permission_required").await?;

    auth.update_user_permissions("uploader", PermissionSet::new(true, false, false))
        .await
        .context("grant User Upload Permission")?;
    let allowed = app
        .oneshot(json_request_with_cookie(
            "/api/mkdir",
            &serde_json::json!({ "path": "", "name": "allowed" }),
            &cookie,
        )?)
        .await
        .context("send allowed authenticated create request")?;
    assert_eq!(allowed.status(), StatusCode::CREATED);
    Ok(())
}

#[tokio::test]
async fn test_should_allow_only_one_concurrent_upload_for_the_same_name() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;

    let (first, second) = tokio::join!(
        upload_request(app.clone(), "same.txt", b"first"),
        upload_request(app, "same.txt", b"second"),
    );
    let statuses = [first?.status(), second?.status()];
    assert!(statuses.contains(&StatusCode::CREATED));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let content = fs::read(storage_root.path().join("same.txt"))
        .await
        .context("read winning concurrent upload")?;
    assert!(content == b"first" || content == b"second");
    Ok(())
}

#[tokio::test]
async fn test_should_clean_staging_when_upload_request_is_cancelled() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let (content_type, first, second) = delayed_multipart_file("cancelled.txt");
    let chunks = vec![(Duration::ZERO, first), (Duration::from_secs(30), second)];
    let body_stream = stream::unfold(chunks.into_iter(), |mut chunks| async move {
        let (delay, chunk) = chunks.next()?;
        tokio::time::sleep(delay).await;
        Some((Ok::<_, Infallible>(chunk), chunks))
    });
    let upload = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/upload")
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from_stream(body_stream))
                .context("build cancellable upload request")?,
        )
        .await
        .context("send cancellable upload request")
    });

    wait_for_staged_file(storage_root.path()).await?;
    upload.abort();
    let _cancelled = upload.await;
    wait_for_staging_empty(storage_root.path()).await?;
    assert!(!storage_root.path().join("cancelled.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_write_paths_and_staging_directory() -> Result<()> {
    use std::os::unix::fs::symlink;

    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    symlink(outside.path(), storage_root.path().join("linked"))
        .context("create Resource Path symlink")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;

    let mkdir = app
        .clone()
        .oneshot(json_request(
            "/api/mkdir",
            &serde_json::json!({ "path": "linked", "name": "escaped" }),
        )?)
        .await
        .context("send symlink create Directory request")?;
    assert_error(mkdir, StatusCode::BAD_REQUEST, "not_directory").await?;
    assert!(!outside.path().join("escaped").exists());

    symlink(outside.path(), storage_root.path().join(".fh-staging"))
        .context("create staging symlink")?;
    let upload = upload_request(app, "safe.txt", b"content").await?;
    assert_error(upload, StatusCode::INTERNAL_SERVER_ERROR, "upload_failed").await?;
    assert!(!outside.path().join("safe.txt").exists());
    Ok(())
}

async fn app_from_storage_root(storage_root: &Path) -> Result<(axum::Router, AppConfig, TempDir)> {
    app_from_storage_root_with_upload_limits(storage_root, 10 * 1024 * 1024, 100 * 1024 * 1024)
        .await
}

async fn app_from_storage_root_with_upload_limits(
    storage_root: &Path,
    single_file_limit: u64,
    total_upload_limit: u64,
) -> Result<(axum::Router, AppConfig, TempDir)> {
    let config_dir = TempDir::new().context("create temporary config directory")?;
    let database_path = config_dir.path().join("file-hub.sqlite");
    let config_path = config_dir.path().join("file-hub.yaml");
    let config = format!(
        r#"
storage_root: {storage_root:?}
database_path: {database_path:?}
staging_directory_name: ".fh-staging"
server:
  bind_address: "127.0.0.1:0"
  time_zone: "UTC"
limits:
  upload_single_file_size_limit_bytes: {single_file_limit}
  upload_total_size_limit_bytes: {total_upload_limit}
  listing_direct_child_limit: 10
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.to_string_lossy(),
        database_path = database_path.to_string_lossy(),
        single_file_limit = single_file_limit,
        total_upload_limit = total_upload_limit,
    );
    fs::write(&config_path, config)
        .await
        .context("write temporary config file")?;
    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load app config")?;
    let app = build_router(config.clone())
        .await
        .context("build app router")?;
    Ok((app, config, config_dir))
}

async fn grant_anonymous_upload_permission(config: &AppConfig) -> Result<()> {
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await
        .context("grant anonymous Upload Permission")?;
    Ok(())
}

async fn upload_request(
    app: axum::Router,
    filename: &str,
    content: &[u8],
) -> Result<axum::response::Response> {
    let (content_type, body) = multipart_file("", filename, content);
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/upload")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .context("build upload request")?,
    )
    .await
    .context("send upload request")
}

fn json_request(uri: &str, body: &serde_json::Value) -> Result<Request<Body>> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .context("build JSON request")
}

fn json_request_with_cookie(
    uri: &str,
    body: &serde_json::Value,
    cookie: &str,
) -> Result<Request<Body>> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::COOKIE, cookie)
        .body(Body::from(body.to_string()))
        .context("build authenticated JSON request")
}

async fn assert_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
) -> Result<()> {
    assert_eq!(response.status(), status);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read error response")?;
    let body: serde_json::Value = serde_json::from_slice(&body).context("decode error response")?;
    assert_eq!(body.pointer("/error/code"), Some(&code.into()));
    Ok(())
}

fn multipart_file(path: &str, filename: &str, content: &[u8]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "file-hub-test-boundary";
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; \
         name=\"path\"\r\n\r\n{path}\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; \
         name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

fn multipart_two_files() -> (String, Vec<u8>) {
    const BOUNDARY: &str = "file-hub-multiple-files-boundary";
    let body = format!(
        concat!(
            "--{0}\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n\r\n",
            "--{0}\r\nContent-Disposition: form-data; name=\"file\"; ",
            "filename=\"first.txt\"\r\n\r\nfirst\r\n",
            "--{0}\r\nContent-Disposition: form-data; name=\"file\"; ",
            "filename=\"second.txt\"\r\n\r\nsecond\r\n",
            "--{0}--\r\n",
        ),
        BOUNDARY,
    )
    .into_bytes();
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

fn delayed_multipart_file(filename: &str) -> (String, Vec<u8>, Vec<u8>) {
    const BOUNDARY: &str = "file-hub-delayed-boundary";
    let first = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; \
         name=\"path\"\r\n\r\n\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
         filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\npartial"
    )
    .into_bytes();
    let second = format!("-complete\r\n--{BOUNDARY}--\r\n").into_bytes();
    (
        format!("multipart/form-data; boundary={BOUNDARY}"),
        first,
        second,
    )
}

async fn wait_for_staged_file(storage_root: &Path) -> Result<()> {
    let staging = storage_root.join(".fh-staging");
    for _ in 0..50 {
        if let Ok(mut entries) = fs::read_dir(&staging).await
            && entries
                .next_entry()
                .await
                .context("read staged upload entry")?
                .is_some()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("upload did not enter reserved staging")
}

async fn wait_for_staging_empty(storage_root: &Path) -> Result<()> {
    let staging = storage_root.join(".fh-staging");
    for _ in 0..50 {
        let mut entries = fs::read_dir(&staging)
            .await
            .context("open staging directory")?;
        if entries
            .next_entry()
            .await
            .context("read staging directory")?
            .is_none()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("cancelled upload staging file was not removed")
}
