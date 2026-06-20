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
async fn test_should_deny_direct_rename_without_rename_permission() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("original.txt"), b"content")
        .await
        .context("write original File")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let response = rename_request(app, "original.txt", "renamed.txt").await?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "rename_permission_required",
    )
    .await?;
    assert!(storage_root.path().join("original.txt").exists());
    assert!(!storage_root.path().join("renamed.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_check_rename_permission_before_parsing_request_body() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rename")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .context("build malformed unauthorized Rename request")?,
        )
        .await
        .context("send malformed unauthorized Rename request")?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "rename_permission_required",
    )
    .await
}

#[tokio::test]
async fn test_should_not_inherit_anonymous_rename_permission_for_authenticated_user() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("original.txt"), b"content")
        .await
        .context("write original File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.create_user("reader", "reader-password")
        .await
        .context("create User without Rename Permission")?;
    let session = auth
        .login("reader", "reader-password")
        .await
        .context("log in User")?
        .context("credentials should create session")?;
    let cookie = format!("fh_session={}", session.token());

    let response = rename_request_with_cookie(app, "original.txt", "renamed.txt", &cookie).await?;

    assert_error(
        response,
        StatusCode::FORBIDDEN,
        "rename_permission_required",
    )
    .await?;
    assert!(storage_root.path().join("original.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_rename_file_within_its_containing_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create containing Directory")?;
    fs::write(storage_root.path().join("docs/original.txt"), b"content")
        .await
        .context("write original File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app.clone(), "docs/original.txt", "renamed.txt").await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let download = app
        .oneshot(
            Request::builder()
                .uri("/api/download?path=docs/renamed.txt")
                .body(Body::empty())
                .context("build renamed File download request")?,
        )
        .await
        .context("download renamed File")?;
    assert_eq!(download.status(), StatusCode::OK);
    let content = to_bytes(download.into_body(), usize::MAX)
        .await
        .context("read renamed File")?;
    assert_eq!(content.as_ref(), b"content");
    assert!(!storage_root.path().join("docs/original.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_rename_directory_within_its_containing_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("docs/original"))
        .await
        .context("create original Directory")?;
    fs::write(
        storage_root.path().join("docs/original/nested.txt"),
        b"nested",
    )
    .await
    .context("write nested File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app.clone(), "docs/original", "renamed").await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let download = app
        .oneshot(
            Request::builder()
                .uri("/api/download?path=docs/renamed/nested.txt")
                .body(Body::empty())
                .context("build nested File download request")?,
        )
        .await
        .context("download File under renamed Directory")?;
    assert_eq!(download.status(), StatusCode::OK);
    assert!(!storage_root.path().join("docs/original").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_treat_same_resource_name_as_successful_noop() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("same.txt"), b"content")
        .await
        .context("write source File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app, "same.txt", "same.txt").await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let content = fs::read(storage_root.path().join("same.txt"))
        .await
        .context("read source File after same-name Rename")?;
    assert_eq!(content, b"content");
    Ok(())
}

#[tokio::test]
async fn test_should_map_invalid_new_resource_name_without_expressing_move() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("original.txt"), b"content")
        .await
        .context("write original File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;
    let response = rename_request(app, "original.txt", "nested/name.txt").await?;
    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "invalid_resource_name",
        "original.txt",
        "Resource Name is invalid",
    )
    .await?;
    assert!(storage_root.path().join("original.txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_name_conflict_without_overwrite_or_auto_rename() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("source.txt"), b"source")
        .await
        .context("write source File")?;
    fs::write(storage_root.path().join("existing.txt"), b"existing")
        .await
        .context("write existing File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app.clone(), "source.txt", "existing.txt").await?;

    assert_write_error(
        response,
        StatusCode::CONFLICT,
        "name_conflict",
        "source.txt",
        "Resource Name conflicts with an existing Resource",
    )
    .await?;
    assert!(storage_root.path().join("source.txt").exists());
    let existing = fs::read(storage_root.path().join("existing.txt"))
        .await
        .context("read existing File after conflict")?;
    assert_eq!(existing, b"existing");
    assert!(!storage_root.path().join("existing (1).txt").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_allow_only_one_concurrent_rename_to_the_same_name() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("first.txt"), b"first")
        .await
        .context("write first source File")?;
    fs::write(storage_root.path().join("second.txt"), b"second")
        .await
        .context("write second source File")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let (first, second) = tokio::join!(
        rename_request(app.clone(), "first.txt", "winner.txt"),
        rename_request(app, "second.txt", "winner.txt"),
    );

    let statuses = [first?.status(), second?.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::NO_CONTENT)
            .count(),
        1,
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1,
    );
    let winner = fs::read(storage_root.path().join("winner.txt"))
        .await
        .context("read winning File")?;
    assert!(winner == b"first" || winner == b"second");
    assert!(
        storage_root.path().join("first.txt").exists()
            || storage_root.path().join("second.txt").exists()
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_root_directory_rename_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app, "", "renamed-root").await?;

    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "root_directory_rename",
        "",
        "Root Directory cannot be renamed",
    )
    .await
}

#[tokio::test]
async fn test_should_reject_rename_path_traversal_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app, "../outside.txt", "renamed.txt").await?;

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
async fn test_should_reject_missing_rename_target_with_clear_write_failure() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app, "missing.txt", "renamed.txt").await?;

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
async fn test_should_reject_symlink_rename_target_with_clear_write_failure() -> Result<()> {
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
    .context("create symlink Resource")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_rename_permission(&config).await?;

    let response = rename_request(app, "linked.txt", "renamed.txt").await?;

    assert_write_error(
        response,
        StatusCode::BAD_REQUEST,
        "invalid_resource_path",
        "linked.txt",
        "resource path is invalid",
    )
    .await?;
    assert!(storage_root.path().join("linked.txt").exists());
    assert!(!storage_root.path().join("renamed.txt").exists());
    assert!(outside.path().join("outside.txt").exists());
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

async fn rename_request(
    app: axum::Router,
    path: &str,
    new_name: &str,
) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/rename")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "path": path, "newName": new_name }).to_string(),
            ))
            .context("build Rename request")?,
    )
    .await
    .context("send Rename request")
}

async fn rename_request_with_cookie(
    app: axum::Router,
    path: &str,
    new_name: &str,
    cookie: &str,
) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/rename")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, cookie)
            .body(Body::from(
                serde_json::json!({ "path": path, "newName": new_name }).to_string(),
            ))
            .context("build authenticated Rename request")?,
    )
    .await
    .context("send authenticated Rename request")
}

async fn grant_anonymous_rename_permission(config: &AppConfig) -> Result<()> {
    let auth = AuthState::connect_existing(config.database_path())
        .await
        .context("connect authentication state")?;
    auth.set_anonymous_permissions(PermissionSet::new(false, true, false))
        .await
        .context("grant anonymous Rename Permission")?;
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
