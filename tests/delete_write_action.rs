use std::path::Path;

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
use tempfile::TempDir;
use tokio::fs;
use tower::ServiceExt;

#[tokio::test]
async fn test_should_deny_direct_delete_without_delete_permission() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("target.txt"), b"content")
        .await
        .context("write target File")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let response = delete_request(app, "target.txt").await?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "delete_permission_required",
    )
    .await?;
    assert!(storage_root.path().join("target.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_authorize_delete_before_parsing_request_body() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/delete")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("not valid JSON"))
                .context("build malformed unauthorized Delete request")?,
        )
        .await
        .context("send malformed unauthorized Delete request")?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "delete_permission_required",
    )
    .await
}

#[tokio::test]
async fn test_should_not_inherit_anonymous_delete_permission_for_authenticated_user() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("target.txt"), b"content")
        .await
        .context("write target File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.create_user("reader", "reader-password")
        .await
        .context("create User without Delete Permission")?;
    let session = auth
        .login("reader", "reader-password")
        .await
        .context("log in User")?
        .context("credentials should create session")?;
    let cookie = format!("fh_session={}", session.token());

    let response = delete_request_with_cookie(app, "target.txt", &cookie).await?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "delete_permission_required",
    )
    .await?;
    assert!(storage_root.path().join("target.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_delete_file_resource() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("target.txt"), b"content")
        .await
        .context("write target File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app.clone(), "target.txt").await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let download = app
        .oneshot(
            Request::builder()
                .uri("/api/download?path=target.txt")
                .body(Body::empty())
                .context("build deleted File download request")?,
        )
        .await
        .context("download deleted File")?;
    assert_eq!(download.status(), StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn test_should_recursively_delete_directory_and_all_contained_resources() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("obsolete/nested/deep"))
        .await
        .context("create nested Directory tree")?;
    fs::write(storage_root.path().join("obsolete/root.txt"), b"root")
        .await
        .context("write root File")?;
    fs::write(
        storage_root.path().join("obsolete/nested/deep/leaf.txt"),
        b"leaf",
    )
    .await
    .context("write nested File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "obsolete").await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!storage_root.path().join("obsolete").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_root_directory_delete_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "").await?;

    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "root_directory_delete",
        "",
        "Root Directory cannot be deleted",
    )
    .await
}

#[tokio::test]
async fn test_should_reject_delete_path_traversal_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "../outside.txt").await?;

    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "invalid_resource_path",
        "../outside.txt",
        "resource path is invalid",
    )
    .await
}

#[tokio::test]
async fn test_should_reject_missing_delete_target_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "missing.txt").await?;

    assert_write_error(
        response,
        StatusCode::NOT_FOUND,
        "resource_not_found",
        "missing.txt",
        "resource path does not exist",
    )
    .await
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_delete_target_with_clear_write_failure() -> Result<()> {
    use std::os::unix::fs::symlink;

    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::write(outside.path().join("outside.txt"), b"outside")
        .await
        .context("write outside File")?;
    symlink(
        outside.path().join("outside.txt"),
        storage_root.path().join("linked.txt"),
    )
    .context("create symlink target")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "linked.txt").await?;

    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "invalid_resource_path",
        "linked.txt",
        "resource path is invalid",
    )
    .await?;
    assert!(storage_root.path().join("linked.txt").exists());
    assert!(outside.path().join("outside.txt").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_fail_fast_on_nested_symlink_without_touching_its_target() -> Result<()> {
    use std::os::unix::fs::symlink;

    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::create_dir(storage_root.path().join("target"))
        .await
        .context("create target Directory")?;
    fs::write(outside.path().join("outside.txt"), b"outside")
        .await
        .context("write outside File")?;
    symlink(outside.path(), storage_root.path().join("target/linked"))
        .context("create nested symlink")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "target").await?;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read nested symlink Delete Failure response")?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).context("decode nested symlink Delete Failure response")?;
    assert_eq!(body.pointer("/error/code"), Some(&"delete_failed".into()));
    assert_eq!(body.pointer("/error/path"), Some(&"target/linked".into()));
    assert!(storage_root.path().join("target/linked").exists());
    assert!(outside.path().join("outside.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_report_first_affected_path_on_partial_recursive_delete_failure() -> Result<()>
{
    use std::os::unix::fs::PermissionsExt;

    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let blocked = storage_root.path().join("target/blocked");
    fs::create_dir_all(&blocked)
        .await
        .context("create blocked Directory")?;
    fs::write(blocked.join("nested.txt"), b"content")
        .await
        .context("write blocked nested File")?;
    fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000))
        .await
        .context("remove blocked Directory permissions")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_delete_permission(&config).await?;

    let response = delete_request(app, "target").await?;

    fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700))
        .await
        .context("restore blocked Directory permissions")?;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read partial Delete Failure response")?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).context("decode partial Delete Failure response")?;
    assert_eq!(body.pointer("/error/code"), Some(&"delete_failed".into()));
    assert_eq!(body.pointer("/error/path"), Some(&"target/blocked".into()));
    let reason = body
        .pointer("/error/reason")
        .and_then(serde_json::Value::as_str)
        .context("partial Delete Failure should contain a reason")?;
    assert!(
        reason.contains("failed to open Directory") || reason.contains("failed to read Directory"),
        "unexpected partial Delete Failure reason: {reason}",
    );
    assert!(storage_root.path().join("target").exists());
    Ok(())
}

async fn app_from_storage_root(storage_root: &Path) -> Result<(axum::Router, AppConfig, TempDir)> {
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
  upload_single_file_size_limit_bytes: 10485760
  upload_total_size_limit_bytes: 104857600
  listing_direct_child_limit: 100
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.to_string_lossy(),
        database_path = database_path.to_string_lossy(),
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

async fn delete_request(app: axum::Router, path: &str) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/delete")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({ "path": path }).to_string()))
            .context("build Delete request")?,
    )
    .await
    .context("send Delete request")
}

async fn delete_request_with_cookie(
    app: axum::Router,
    path: &str,
    cookie: &str,
) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/delete")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie)
            .body(Body::from(serde_json::json!({ "path": path }).to_string()))
            .context("build authenticated Delete request")?,
    )
    .await
    .context("send authenticated Delete request")
}

async fn grant_anonymous_delete_permission(config: &AppConfig) -> Result<()> {
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(false, false, true))
        .await
        .context("grant anonymous Delete Permission")?;
    Ok(())
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

async fn assert_write_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
    path: &str,
    reason: &str,
) -> Result<()> {
    assert_eq!(response.status(), status);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read Write Failure response")?;
    let body: serde_json::Value =
        serde_json::from_slice(&body).context("decode Write Failure response")?;
    assert_eq!(body.pointer("/error/code"), Some(&code.into()));
    assert_eq!(body.pointer("/error/path"), Some(&path.into()));
    assert_eq!(body.pointer("/error/reason"), Some(&reason.into()));
    Ok(())
}
