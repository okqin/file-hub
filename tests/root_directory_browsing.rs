use std::path::Path;

use anyhow::{Context, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use file_hub::{config::AppConfig, http::build_router};
use filetime::{FileTime, set_file_mtime};
use serde_json::Value;
use tempfile::TempDir;
use tokio::fs;
use tower::ServiceExt;

#[tokio::test]
async fn test_should_list_root_directory_for_anonymous_http_request() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;
    fs::write(storage_root.path().join("readme.txt"), b"hello")
        .await
        .context("write readme file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_root_listing(app).await?;
    let names = resource_names(&body)?;

    assert_eq!(names, vec!["docs", "readme.txt"]);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_exclude_symbolic_links_from_root_directory_listing() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("target.txt"), b"target")
        .await
        .context("write symlink target")?;
    tokio::fs::symlink(
        storage_root.path().join("target.txt"),
        storage_root.path().join("link.txt"),
    )
    .await
    .context("create symlink")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_root_listing(app).await?;
    let names = resource_names(&body)?;

    assert_eq!(names, vec!["target.txt"]);
    Ok(())
}

#[tokio::test]
async fn test_should_show_leading_dot_resources_except_reserved_staging_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join(".gitignore"), b"target\n")
        .await
        .context("write leading-dot file")?;
    fs::create_dir(storage_root.path().join(".fh-staging"))
        .await
        .context("create reserved staging directory")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_root_listing(app).await?;
    let names = resource_names(&body)?;

    assert_eq!(names, vec![".gitignore"]);
    Ok(())
}

#[tokio::test]
async fn test_should_return_directory_first_rows_with_file_size_mtime_and_relative_paths()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("zeta-dir"))
        .await
        .context("create zeta directory")?;
    fs::create_dir(storage_root.path().join("alpha-dir"))
        .await
        .context("create alpha directory")?;
    fs::write(storage_root.path().join("zeta.txt"), b"zeta")
        .await
        .context("write zeta file")?;
    let alpha_file = storage_root.path().join("alpha.txt");
    fs::write(&alpha_file, b"alpha")
        .await
        .context("write alpha file")?;
    set_file_mtime(&alpha_file, FileTime::from_unix_time(1_700_000_000, 0))
        .context("set alpha file mtime")?;

    let app = app_from_storage_root(storage_root.path(), 10, "Asia/Shanghai").await?;
    let body = get_root_listing(app).await?;
    let names = resource_names(&body)?;

    assert_eq!(
        names,
        vec!["alpha-dir", "zeta-dir", "alpha.txt", "zeta.txt"]
    );

    let alpha_dir = resource_named(&body, "alpha-dir")?;
    assert_eq!(
        alpha_dir.get("kind"),
        Some(&Value::String("directory".to_owned()))
    );
    assert!(alpha_dir.get("size").is_none());
    assert_eq!(
        alpha_dir.get("resourcePath"),
        Some(&Value::String("alpha-dir".to_owned()))
    );

    let alpha_file = resource_named(&body, "alpha.txt")?;
    assert_eq!(
        alpha_file.get("kind"),
        Some(&Value::String("file".to_owned()))
    );
    assert_eq!(alpha_file.get("size"), Some(&Value::Number(5.into())));
    assert_eq!(
        alpha_file.get("modifiedTime"),
        Some(&Value::String("2023-11-15 06:13:20".to_owned()))
    );
    assert_eq!(
        alpha_file.get("resourcePath"),
        Some(&Value::String("alpha.txt".to_owned()))
    );

    let body_text = serde_json::to_string(&body).context("serialize body for path assertion")?;
    assert!(!body_text.contains(&storage_root.path().to_string_lossy().to_string()));
    Ok(())
}

#[tokio::test]
async fn test_should_fail_clearly_without_partial_rows_when_listing_limit_is_exceeded() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("one.txt"), b"one")
        .await
        .context("write first file")?;
    fs::write(storage_root.path().join("two.txt"), b"two")
        .await
        .context("write second file")?;

    let app = app_from_storage_root(storage_root.path(), 1, "UTC").await?;
    let response = request_root_listing(app).await?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read error response body")?;
    let body: Value = serde_json::from_slice(&body).context("parse error response body")?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&Value::String("listing_limit_exceeded".to_owned()))
    );
    assert!(body.get("resources").is_none());
    Ok(())
}

#[tokio::test]
async fn test_should_reject_invalid_configuration_before_serving_traffic() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_dir = TempDir::new().context("create temporary config directory")?;
    let config_path = config_dir.path().join("file-hub.yaml");
    let config = format!(
        r#"
storage_root: {storage_root:?}
server:
  bind_address: "127.0.0.1:0"
  time_zone: "Not/AZone"
limits:
  listing_direct_child_limit: 10
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.path().to_string_lossy(),
    );
    fs::write(&config_path, config)
        .await
        .context("write invalid config file")?;

    let result = AppConfig::load_from_path(&config_path).await;

    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
async fn test_should_render_browser_page_that_loads_root_directory_listing() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("readme.txt"), b"hello")
        .await
        .context("write readme file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/")
                .body(Body::empty())
                .context("build browser request")?,
        )
        .await
        .context("send browser request")?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read browser response body")?;
    let body = String::from_utf8(body.to_vec()).context("browser body must be UTF-8")?;

    assert!(body.contains("aria-label=\"Root Directory\""));
    assert!(body.contains("id=\"resources\""));
    assert!(body.contains("fetch('/api/list')"));
    Ok(())
}

async fn app_from_storage_root(
    storage_root: &Path,
    listing_limit: usize,
    time_zone: &str,
) -> Result<axum::Router> {
    let config_dir = TempDir::new().context("create temporary config directory")?;
    let config_path = config_dir.path().join("file-hub.yaml");
    let config = format!(
        r#"
storage_root: {storage_root:?}
staging_directory_name: ".fh-staging"
server:
  bind_address: "127.0.0.1:0"
  time_zone: "{time_zone}"
limits:
  listing_direct_child_limit: {listing_limit}
  request_timeout_seconds: 5
  fs_concurrency_limit: 4
"#,
        storage_root = storage_root.to_string_lossy(),
    );
    fs::write(&config_path, config)
        .await
        .context("write temporary config file")?;

    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load app config")?;
    Ok(build_router(config))
}

fn resource_names(body: &Value) -> Result<Vec<&str>> {
    let resources = body
        .get("resources")
        .and_then(Value::as_array)
        .context("resources must be an array")?;

    resources
        .iter()
        .map(|resource| {
            resource
                .get("name")
                .and_then(Value::as_str)
                .context("resource name must be a string")
        })
        .collect()
}

fn resource_named<'a>(body: &'a Value, name: &str) -> Result<&'a Value> {
    let resources = body
        .get("resources")
        .and_then(Value::as_array)
        .context("resources must be an array")?;
    resources
        .iter()
        .find(|resource| resource.get("name").and_then(Value::as_str) == Some(name))
        .with_context(|| format!("resource {name} must exist"))
}

async fn get_root_listing(app: axum::Router) -> Result<Value> {
    let response = request_root_listing(app).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read list response body")?;
    serde_json::from_slice(&body).context("parse list response body")
}

async fn request_root_listing(app: axum::Router) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri("/api/list")
            .body(Body::empty())
            .context("build list request")?,
    )
    .await
    .context("send list request")
}
