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
async fn test_should_upload_complete_nested_directory_tree_atomically() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let files = [
        ("selected/readme.txt", b"root file".as_slice()),
        ("selected/docs/guide.txt", b"nested file".as_slice()),
    ];

    let response = directory_upload_request(app.clone(), "", &files).await?;

    let status = response.status();
    let response_body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "Directory Upload failed: {}",
        String::from_utf8_lossy(&response_body),
    );
    assert_eq!(
        fs::read(storage_root.path().join("selected/readme.txt")).await?,
        b"root file",
    );
    assert_eq!(
        fs::read(storage_root.path().join("selected/docs/guide.txt")).await?,
        b"nested file",
    );
    let listing = app
        .oneshot(
            Request::builder()
                .uri("/api/list")
                .body(Body::empty())
                .context("build Root Directory listing request")?,
        )
        .await
        .context("list Root Directory after Directory Upload")?;
    let body = to_bytes(listing.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(body.pointer("/resources/0/name"), Some(&"selected".into()));
    assert_eq!(body.pointer("/resources/0/kind"), Some(&"directory".into()));
    Ok(())
}

#[tokio::test]
async fn test_should_reject_invalid_nested_resource_name_with_failure_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/docs/bad\nname.txt";
    let files = [
        ("selected/good.txt", b"good".as_slice()),
        (failed_path, b"bad"),
    ];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"invalid_directory_upload_path".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert_eq!(
        body.pointer("/error/reason"),
        Some(&"relative path contains an invalid Resource Name".into()),
    );
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_directory_upload_conflict_without_overwriting() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("selected")).await?;
    fs::write(
        storage_root.path().join("selected/original.txt"),
        b"original",
    )
    .await?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let files = [("selected/replacement.txt", b"replacement".as_slice())];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"directory_upload_conflict".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&"selected".into()));
    assert_eq!(
        fs::read(storage_root.path().join("selected/original.txt")).await?,
        b"original",
    );
    assert!(
        !storage_root
            .path()
            .join("selected/replacement.txt")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn test_should_publish_only_one_of_two_concurrent_directory_uploads() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let first = [("selected/file.txt", b"first".as_slice())];
    let second = [("selected/file.txt", b"second".as_slice())];

    let (first_response, second_response) = tokio::join!(
        directory_upload_request(app.clone(), "", &first),
        directory_upload_request(app, "", &second),
    );

    let statuses = [first_response?.status(), second_response?.status()];
    assert!(statuses.contains(&StatusCode::CREATED));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let content = fs::read(storage_root.path().join("selected/file.txt")).await?;
    assert!(content == b"first" || content == b"second");
    Ok(())
}

#[tokio::test]
async fn test_should_reject_directory_upload_path_traversal_without_staging_escape() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/../escaped.txt";
    let files = [(failed_path, b"escaped".as_slice())];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    assert!(!storage_root.path().join("escaped.txt").exists());
    assert_staging_empty(storage_root.path()).await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_directory_upload_destination() -> Result<()> {
    use std::os::unix::fs::symlink;

    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    symlink(outside.path(), storage_root.path().join("selected"))
        .context("create destination symlink")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let files = [("selected/file.txt", b"content".as_slice())];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!outside.path().join("file.txt").exists());
    assert_staging_empty(storage_root.path()).await?;
    Ok(())
}

#[tokio::test]
async fn test_should_clean_staged_directory_when_request_is_cancelled() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let (content_type, first, second) = delayed_multipart_directory();
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
                .context("build cancellable Directory Upload request")?,
        )
        .await
        .context("send cancellable Directory Upload request")
    });

    wait_for_staged_directory(storage_root.path()).await?;
    upload.abort();
    let _cancelled = upload.await;
    wait_for_staging_empty(storage_root.path()).await?;
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_oversized_file_before_publishing_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) =
        app_from_storage_root_with_upload_limits(storage_root.path(), 3, 100).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/large.txt";
    let files = [(failed_path, b"four".as_slice())];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"directory_upload_single_file_size_limit_exceeded".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_directory_total_size_limit_with_failure_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) =
        app_from_storage_root_with_upload_limits(storage_root.path(), 10, 5).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/second.txt";
    let files = [
        ("selected/first.txt", b"one".as_slice()),
        (failed_path, b"two".as_slice()),
    ];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"directory_upload_total_size_limit_exceeded".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_directory_resource_count_limit_with_failure_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) =
        app_from_storage_root_with_limits(storage_root.path(), 10, 100, 3).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/docs/second.txt";
    let files = [
        ("selected/first.txt", b"one".as_slice()),
        (failed_path, b"two".as_slice()),
    ];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"directory_upload_resource_count_limit_exceeded".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_clean_stale_staging_remnants_on_startup_without_touching_resources()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("kept-directory")).await?;
    fs::write(
        storage_root.path().join("kept-directory/user.txt"),
        b"user resource",
    )
    .await?;
    fs::create_dir_all(storage_root.path().join(".fh-staging/stale-tree/nested")).await?;
    fs::write(
        storage_root
            .path()
            .join(".fh-staging/stale-tree/nested/partial.txt"),
        b"partial",
    )
    .await?;

    let (_app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    assert!(!storage_root.path().join(".fh-staging/stale-tree").exists());
    assert_eq!(
        fs::read(storage_root.path().join("kept-directory/user.txt")).await?,
        b"user resource",
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_conflicting_paths_inside_uploaded_tree_and_roll_back() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/docs/guide.txt";
    let files = [
        ("selected/docs", b"file named docs".as_slice()),
        (failed_path, b"nested file".as_slice()),
    ];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&"directory_upload_conflict".into()),
    );
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_render_permission_gated_directory_upload_with_overall_progress() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, _config, _config_dir) = app_from_storage_root(storage_root.path()).await?;

    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .context("build browser request")?,
        )
        .await
        .context("request browser page")?;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body = String::from_utf8(body.to_vec()).context("browser page must be UTF-8")?;

    assert!(body.contains("id=\"upload-directory-action\" hidden"));
    assert!(body.contains("id=\"upload-directory-input\" webkitdirectory hidden"));
    assert!(body.contains("uploadDirectoryAction.hidden = !identity.actions.upload"));
    assert!(body.contains("form.append('relativePath', file.webkitRelativePath)"));
    assert!(body.contains("request.upload.onprogress"));
    assert!(body.contains("uploadProgress.value = event.lengthComputable && event.total"));
    Ok(())
}

#[tokio::test]
async fn test_should_reject_reserved_staging_name_anywhere_in_directory_upload() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let (app, config, _config_dir) = app_from_storage_root(storage_root.path()).await?;
    grant_anonymous_upload_permission(&config).await?;
    let failed_path = "selected/.fh-staging/hidden.txt";
    let files = [(failed_path, b"hidden".as_slice())];

    let response = directory_upload_request(app, "", &files).await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let body: serde_json::Value = serde_json::from_slice(&body)?;
    assert_eq!(body.pointer("/error/path"), Some(&failed_path.into()));
    assert!(!storage_root.path().join("selected").exists());
    Ok(())
}

#[tokio::test]
async fn test_should_preserve_default_database_while_cleaning_staging_on_restart() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_dir = TempDir::new().context("create temporary config directory")?;
    let config_path = config_dir.path().join("file-hub.yaml");
    let config_text = format!(
        r#"
storage_root: {storage_root:?}
staging_directory_name: ".fh-staging"
server:
  bind_address: "127.0.0.1:0"
  time_zone: "UTC"
limits:
  upload_single_file_size_limit_bytes: 10
  upload_total_size_limit_bytes: 100
  directory_upload_resource_count_limit: 100
  listing_direct_child_limit: 100
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.path().to_string_lossy(),
    );
    fs::write(&config_path, config_text).await?;
    let config = AppConfig::load_from_path(&config_path).await?;
    let _first_app = build_router(config.clone()).await?;
    let auth = AuthState::connect_existing(config.database_path()).await?;
    auth.create_user_with_permissions(
        "preserved-user",
        "preserved-password",
        PermissionSet::new(true, false, false),
    )
    .await?;
    fs::create_dir_all(storage_root.path().join(".fh-staging/stale-upload")).await?;
    fs::write(
        storage_root.path().join(".fh-staging/stale-upload/partial"),
        b"partial",
    )
    .await?;

    let _second_app = build_router(config.clone()).await?;

    let auth = AuthState::connect_existing(config.database_path()).await?;
    assert!(
        auth.login("preserved-user", "preserved-password")
            .await?
            .is_some()
    );
    assert!(
        !storage_root
            .path()
            .join(".fh-staging/stale-upload")
            .exists()
    );
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
    app_from_storage_root_with_limits(storage_root, single_file_limit, total_upload_limit, 10_000)
        .await
}

async fn app_from_storage_root_with_limits(
    storage_root: &Path,
    single_file_limit: u64,
    total_upload_limit: u64,
    directory_resource_count_limit: usize,
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
  directory_upload_resource_count_limit: {directory_resource_count_limit}
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
        single_file_limit = single_file_limit,
        total_upload_limit = total_upload_limit,
        directory_resource_count_limit = directory_resource_count_limit,
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
    let auth = AuthState::connect_existing(config.database_path()).await?;
    auth.set_anonymous_permissions(PermissionSet::new(true, false, false))
        .await?;
    Ok(())
}

async fn directory_upload_request(
    app: axum::Router,
    path: &str,
    files: &[(&str, &[u8])],
) -> Result<axum::response::Response> {
    let (content_type, body) = multipart_directory(path, files);
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/upload")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .context("build Directory Upload request")?,
    )
    .await
    .context("send Directory Upload request")
}

fn multipart_directory(path: &str, files: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "file-hub-directory-upload-boundary";
    let mut body =
        format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n{path}\r\n")
            .into_bytes();
    for (relative_path, content) in files {
        body.extend_from_slice(
            format!(
                concat!(
                    "--{}\r\n",
                    "Content-Disposition: form-data; name=\"relativePath\"\r\n\r\n",
                    "{}\r\n",
                    "--{}\r\n",
                    "Content-Disposition: form-data; name=\"file\"; filename=\"file\"\r\n",
                    "Content-Type: application/octet-stream\r\n\r\n",
                ),
                BOUNDARY, relative_path, BOUNDARY
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={BOUNDARY}"), body)
}

fn delayed_multipart_directory() -> (String, Vec<u8>, Vec<u8>) {
    const BOUNDARY: &str = "file-hub-delayed-directory-boundary";
    let first = format!(
        concat!(
            "--{0}\r\nContent-Disposition: form-data; name=\"path\"\r\n\r\n\r\n",
            "--{0}\r\nContent-Disposition: form-data; name=\"relativePath\"\r\n\r\n",
            "selected/file.txt\r\n",
            "--{0}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"file.txt\"\r\n",
            "Content-Type: application/octet-stream\r\n\r\npartial",
        ),
        BOUNDARY,
    )
    .into_bytes();
    let second = format!("-complete\r\n--{BOUNDARY}--\r\n").into_bytes();
    (
        format!("multipart/form-data; boundary={BOUNDARY}"),
        first,
        second,
    )
}

async fn wait_for_staged_directory(storage_root: &Path) -> Result<()> {
    let staging = storage_root.join(".fh-staging");
    for _ in 0..50 {
        if let Ok(mut entries) = fs::read_dir(&staging).await
            && entries
                .next_entry()
                .await
                .context("read staged Directory Upload entry")?
                .is_some()
        {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("Directory Upload did not enter reserved staging")
}

async fn wait_for_staging_empty(storage_root: &Path) -> Result<()> {
    for _ in 0..50 {
        if assert_staging_empty(storage_root).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("Directory Upload staging remnant was not removed")
}

async fn assert_staging_empty(storage_root: &Path) -> Result<()> {
    let mut entries = fs::read_dir(storage_root.join(".fh-staging"))
        .await
        .context("open staging directory")?;
    anyhow::ensure!(
        entries
            .next_entry()
            .await
            .context("read staging directory")?
            .is_none(),
        "staging directory is not empty",
    );
    Ok(())
}
