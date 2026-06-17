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

#[tokio::test]
async fn test_should_open_directory_resource_and_reload_direct_listing() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;
    fs::write(storage_root.path().join("docs").join("guide.txt"), b"guide")
        .await
        .context("write nested guide file")?;
    fs::write(storage_root.path().join("root.txt"), b"root")
        .await
        .context("write root file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_listing(app, Some("docs")).await?;
    let names = resource_names(&body)?;

    assert_eq!(body.get("path"), Some(&Value::String("docs".to_owned())));
    assert_eq!(names, vec!["guide.txt"]);
    let guide = resource_named(&body, "guide.txt")?;
    assert_eq!(
        guide.get("resourcePath"),
        Some(&Value::String("docs/guide.txt".to_owned()))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_return_breadcrumb_segments_for_current_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("docs").join("guides"))
        .await
        .context("create nested directory")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_listing(app, Some("docs/guides")).await?;
    let breadcrumb_labels = breadcrumb_labels(&body)?;
    let breadcrumb_paths = breadcrumb_paths(&body)?;

    assert_eq!(breadcrumb_labels, vec!["Root Directory", "docs", "guides"]);
    assert_eq!(breadcrumb_paths, vec!["", "docs", "docs/guides"]);
    Ok(())
}

#[tokio::test]
async fn test_should_sort_by_file_size_descending_with_directories_first() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("beta-dir"))
        .await
        .context("create beta directory")?;
    fs::create_dir(storage_root.path().join("alpha-dir"))
        .await
        .context("create alpha directory")?;
    fs::write(storage_root.path().join("small.txt"), b"1")
        .await
        .context("write small file")?;
    fs::write(storage_root.path().join("large.txt"), b"12345")
        .await
        .context("write large file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_listing_with_query(app, "sort=size&order=desc").await?;
    let names = resource_names(&body)?;

    assert_eq!(
        names,
        vec!["alpha-dir", "beta-dir", "large.txt", "small.txt"]
    );
    assert_eq!(
        body.pointer("/sort/field"),
        Some(&Value::String("size".to_owned()))
    );
    assert_eq!(
        body.pointer("/sort/order"),
        Some(&Value::String("desc".to_owned()))
    );
    assert!(resource_named(&body, "alpha-dir")?.get("size").is_none());
    Ok(())
}

#[tokio::test]
async fn test_should_sort_by_modified_time_descending_with_directories_first() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let old_dir = storage_root.path().join("old-dir");
    let new_dir = storage_root.path().join("new-dir");
    let old_file = storage_root.path().join("old.txt");
    let new_file = storage_root.path().join("new.txt");
    fs::create_dir(&old_dir)
        .await
        .context("create old directory")?;
    fs::create_dir(&new_dir)
        .await
        .context("create new directory")?;
    fs::write(&old_file, b"old")
        .await
        .context("write old file")?;
    fs::write(&new_file, b"new")
        .await
        .context("write new file")?;
    set_file_mtime(&old_dir, FileTime::from_unix_time(1_700_000_000, 0))
        .context("set old directory mtime")?;
    set_file_mtime(&new_dir, FileTime::from_unix_time(1_800_000_000, 0))
        .context("set new directory mtime")?;
    set_file_mtime(&old_file, FileTime::from_unix_time(1_700_000_000, 0))
        .context("set old file mtime")?;
    set_file_mtime(&new_file, FileTime::from_unix_time(1_800_000_000, 0))
        .context("set new file mtime")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_listing_with_query(app, "sort=modifiedTime&order=desc").await?;
    let names = resource_names(&body)?;

    assert_eq!(names, vec!["new-dir", "old-dir", "new.txt", "old.txt"]);
    assert_eq!(
        body.pointer("/sort/field"),
        Some(&Value::String("modifiedTime".to_owned()))
    );
    assert_eq!(
        body.pointer("/sort/order"),
        Some(&Value::String("desc".to_owned()))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_filter_current_directory_only_while_preserving_sort() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let docs = storage_root.path().join("docs");
    fs::create_dir(&docs)
        .await
        .context("create docs directory")?;
    fs::create_dir(docs.join("nested"))
        .await
        .context("create nested directory")?;
    fs::write(docs.join("guide-a.txt"), b"a")
        .await
        .context("write first direct match")?;
    fs::write(docs.join("guide-z.txt"), b"z")
        .await
        .context("write second direct match")?;
    fs::write(docs.join("notes.txt"), b"notes")
        .await
        .context("write non-matching direct file")?;
    fs::write(docs.join("nested").join("guide-nested.txt"), b"nested")
        .await
        .context("write nested match")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let body = get_listing_with_query(app, "path=docs&filter=guide&sort=name&order=desc").await?;
    let names = resource_names(&body)?;

    assert_eq!(names, vec!["guide-z.txt", "guide-a.txt"]);
    assert_eq!(
        body.pointer("/filter/query"),
        Some(&Value::String("guide".to_owned()))
    );
    assert_eq!(
        body.pointer("/sort/order"),
        Some(&Value::String("desc".to_owned()))
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_path_traversal_and_absolute_resource_paths() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;

    let traversal = request_listing_with_query(app.clone(), "path=..").await?;
    assert_error(traversal, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;

    let absolute = request_listing_with_query(app, "path=%2Ftmp").await?;
    assert_error(absolute, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_direct_access_to_reserved_staging_directory() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join(".fh-staging"))
        .await
        .context("create reserved staging directory")?;
    fs::write(
        storage_root.path().join(".fh-staging").join("internal.txt"),
        b"internal",
    )
    .await
    .context("write staging internal file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_listing(app, Some(".fh-staging")).await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_return_not_found_for_missing_directory_resource_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_listing(app, Some("missing")).await?;

    assert_error(response, StatusCode::NOT_FOUND, "resource_not_found").await?;
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

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_directory_resource_open() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::write(outside.path().join("secret.txt"), b"secret")
        .await
        .context("write outside file")?;
    tokio::fs::symlink(outside.path(), storage_root.path().join("outside-link"))
        .await
        .context("create symlink directory")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_listing(app, Some("outside-link")).await?;

    assert_error(response, StatusCode::BAD_REQUEST, "not_directory").await?;
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
    assert!(body.contains("fetch('/api/list'"));
    Ok(())
}

#[tokio::test]
async fn test_should_render_browser_controls_for_navigation_sort_and_current_filter() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

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

    assert!(body.contains("aria-label=\"Breadcrumb\""));
    assert!(body.contains("id=\"return-to-parent\""));
    assert!(body.contains("id=\"current-list-filter\""));
    assert!(body.contains("data-sort-field=\"name\""));
    assert!(body.contains("data-sort-field=\"size\""));
    assert!(body.contains("data-sort-field=\"modifiedTime\""));
    assert!(body.contains("Current List Filter"));
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

fn breadcrumb_labels(body: &Value) -> Result<Vec<&str>> {
    breadcrumb_values(body, "label")
}

fn breadcrumb_paths(body: &Value) -> Result<Vec<&str>> {
    breadcrumb_values(body, "path")
}

fn breadcrumb_values<'a>(body: &'a Value, field: &str) -> Result<Vec<&'a str>> {
    let breadcrumbs = body
        .get("breadcrumbs")
        .and_then(Value::as_array)
        .context("breadcrumbs must be an array")?;

    breadcrumbs
        .iter()
        .map(|breadcrumb| {
            breadcrumb
                .get(field)
                .and_then(Value::as_str)
                .with_context(|| format!("breadcrumb {field} must be a string"))
        })
        .collect()
}

async fn get_root_listing(app: axum::Router) -> Result<Value> {
    get_listing(app, None).await
}

async fn get_listing(app: axum::Router, path: Option<&str>) -> Result<Value> {
    let response = request_listing(app, path).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read list response body")?;
    serde_json::from_slice(&body).context("parse list response body")
}

async fn get_listing_with_query(app: axum::Router, query: &str) -> Result<Value> {
    let response = request_listing_with_query(app, query).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read list response body")?;
    serde_json::from_slice(&body).context("parse list response body")
}

async fn request_listing_with_query(
    app: axum::Router,
    query: &str,
) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri(format!("/api/list?{query}"))
            .body(Body::empty())
            .context("build list request")?,
    )
    .await
    .context("send list request")
}

async fn request_root_listing(app: axum::Router) -> Result<axum::response::Response> {
    request_listing(app, None).await
}

async fn request_listing(
    app: axum::Router,
    path: Option<&str>,
) -> Result<axum::response::Response> {
    let uri = match path {
        Some(path) => format!("/api/list?path={path}"),
        None => "/api/list".to_owned(),
    };

    app.oneshot(
        Request::builder()
            .uri(uri)
            .body(Body::empty())
            .context("build list request")?,
    )
    .await
    .context("send list request")
}

async fn assert_error(
    response: axum::response::Response,
    status: StatusCode,
    code: &str,
) -> Result<()> {
    assert_eq!(response.status(), status);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read error response body")?;
    let body: Value = serde_json::from_slice(&body).context("parse error response body")?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&Value::String(code.to_owned()))
    );
    assert!(body.pointer("/error/reason").is_some());
    assert!(body.get("resources").is_none());
    Ok(())
}
