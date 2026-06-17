use std::{io::Read, path::Path};

use anyhow::{Context, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
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
async fn test_should_download_file_resource_for_anonymous_http_request() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(
        storage_root.path().join("readme.txt"),
        b"hello from storage root",
    )
    .await
    .context("write downloadable file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "readme.txt").await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&header::HeaderValue::from_static(
            "attachment; filename=\"readme.txt\"; filename*=UTF-8''readme.txt",
        )),
    );
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "application/octet-stream"
        )),
    );
    assert_eq!(
        response.headers().get(header::CONTENT_LENGTH),
        Some(&header::HeaderValue::from_static("23")),
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read download response body")?;
    assert_eq!(body.as_ref(), b"hello from storage root");
    let storage_root_text = storage_root.path().to_string_lossy();
    assert!(!String::from_utf8_lossy(&body).contains(storage_root_text.as_ref()));
    Ok(())
}

#[tokio::test]
async fn test_should_download_directory_archive_with_top_level_directory_for_anonymous_http_request()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("docs").join("guides"))
        .await
        .context("create nested directory")?;
    fs::write(
        storage_root
            .path()
            .join("docs")
            .join("guides")
            .join("setup.txt"),
        b"nested setup",
    )
    .await
    .context("write nested file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_archive(app, "docs").await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&header::HeaderValue::from_static(
            "attachment; filename=\"docs.zip\"; filename*=UTF-8''docs.zip",
        )),
    );
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("application/zip")),
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read archive response body")?;
    let entries = zip_entry_names(body.as_ref())?;

    assert_eq!(
        entries,
        vec!["docs/", "docs/guides/", "docs/guides/setup.txt"]
    );
    assert_eq!(
        zip_entry_text(body.as_ref(), "docs/guides/setup.txt")?,
        "nested setup",
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_root_directory_archive_download() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_archive(app, "").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "root_directory_archive").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_archive_path_traversal_and_absolute_resource_paths() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;

    let traversal = request_archive(app.clone(), "..").await?;
    assert_error(traversal, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;

    let absolute = request_archive(app, "%2Ftmp").await?;
    assert_error(absolute, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_archive_from_reserved_staging_directory() -> Result<()> {
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
    let response = request_archive(app, ".fh-staging").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_file_resource_path_for_directory_archive_download() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("readme.txt"), b"readme")
        .await
        .context("write file resource")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_archive(app, "readme.txt").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "not_directory").await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_directory_resource_archive_download() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::write(outside.path().join("secret.txt"), b"secret")
        .await
        .context("write outside file")?;
    tokio::fs::symlink(outside.path(), storage_root.path().join("outside-link"))
        .await
        .context("create symlink directory")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_archive(app, "outside-link").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "not_directory").await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_exclude_nested_symlinks_from_directory_archive() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;
    fs::write(
        storage_root.path().join("docs").join("visible.txt"),
        b"visible",
    )
    .await
    .context("write visible file")?;
    fs::write(outside.path().join("secret.txt"), b"secret")
        .await
        .context("write outside file")?;
    tokio::fs::symlink(
        outside.path().join("secret.txt"),
        storage_root.path().join("docs").join("secret-link.txt"),
    )
    .await
    .context("create nested symlink")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_archive(app, "docs").await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read archive response body")?;
    let entries = zip_entry_names(body.as_ref())?;

    assert_eq!(entries, vec!["docs/", "docs/visible.txt"]);
    let archive_text = String::from_utf8_lossy(body.as_ref());
    assert!(!archive_text.contains("secret"));
    Ok(())
}

#[tokio::test]
async fn test_should_fail_directory_archive_before_response_bytes_when_resource_count_limit_is_exceeded()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;
    fs::write(storage_root.path().join("docs").join("one.txt"), b"one")
        .await
        .context("write file resource")?;

    let app =
        app_from_storage_root_with_archive_limits(storage_root.path(), 10, 1, 1024, "UTC").await?;
    let response = request_archive(app, "docs").await?;

    assert_error(
        response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "archive_resource_count_limit_exceeded",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn test_should_fail_directory_archive_before_response_bytes_when_size_limit_is_exceeded()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;
    fs::write(storage_root.path().join("docs").join("large.txt"), b"large")
        .await
        .context("write file resource")?;

    let app =
        app_from_storage_root_with_archive_limits(storage_root.path(), 10, 100, 4, "UTC").await?;
    let response = request_archive(app, "docs").await?;

    assert_error(
        response,
        StatusCode::PAYLOAD_TOO_LARGE,
        "archive_size_limit_exceeded",
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn test_should_escape_download_name_in_content_disposition() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(
        storage_root.path().join("report \"final\".txt"),
        b"quoted download",
    )
    .await
    .context("write file with quoted resource name")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "report%20%22final%22.txt").await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_DISPOSITION),
        Some(&header::HeaderValue::from_static(
            "attachment; filename=\"report \\\"final\\\".txt\"; \
             filename*=UTF-8''report%20%22final%22.txt",
        )),
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read quoted download response body")?;
    assert_eq!(body.as_ref(), b"quoted download");
    Ok(())
}

#[tokio::test]
async fn test_should_reject_download_without_resource_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download_without_path(app).await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_empty_download_resource_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_directory_resource_path_for_download() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("docs"))
        .await
        .context("create docs directory")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "docs").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "not_file").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_return_not_found_for_missing_download_resource_path() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "missing.txt").await?;

    assert_error(response, StatusCode::NOT_FOUND, "resource_not_found").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_download_path_traversal_and_absolute_resource_paths() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;

    let traversal = request_download(app.clone(), "..").await?;
    assert_error(traversal, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;

    let absolute = request_download(app, "%2Ftmp").await?;
    assert_error(absolute, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_reject_symlink_file_resource_download() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::write(outside.path().join("secret.txt"), b"secret")
        .await
        .context("write outside file")?;
    tokio::fs::symlink(
        outside.path().join("secret.txt"),
        storage_root.path().join("secret-link.txt"),
    )
    .await
    .context("create symlink file")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_download(app, "secret-link.txt").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "not_file").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_download_from_reserved_staging_directory() -> Result<()> {
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
    let response = request_download(app, ".fh-staging/internal.txt").await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_resource_path").await?;
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
async fn test_should_reject_server_search_query_shorter_than_two_non_whitespace_characters()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let response = request_search(app.clone(), "q=%20a%20").await?;
    assert_error(response, StatusCode::BAD_REQUEST, "invalid_search_query").await?;

    let response = request_search(app, "q=%E5%AD%97").await?;
    assert_error(response, StatusCode::BAD_REQUEST, "invalid_search_query").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_reject_server_search_query_over_configured_length() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;

    let app = app_from_storage_root(storage_root.path(), 10, "UTC").await?;
    let long_query = "a".repeat(257);
    let response = request_search(app, &format!("q={long_query}")).await?;

    assert_error(response, StatusCode::BAD_REQUEST, "invalid_search_query").await?;
    Ok(())
}

#[tokio::test]
async fn test_should_return_flat_nested_server_search_results_with_containing_paths() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("alpha").join("nested"))
        .await
        .context("create alpha nested directory")?;
    fs::create_dir(storage_root.path().join("beta"))
        .await
        .context("create beta directory")?;
    fs::write(
        storage_root.path().join("alpha").join("Report.txt"),
        b"alpha",
    )
    .await
    .context("write alpha report")?;
    fs::write(
        storage_root
            .path()
            .join("alpha")
            .join("nested")
            .join("report.txt"),
        b"nested",
    )
    .await
    .context("write nested report")?;
    fs::write(storage_root.path().join("beta").join("REPORT.txt"), b"beta")
        .await
        .context("write beta report")?;
    fs::write(storage_root.path().join("report-notes.txt"), b"root")
        .await
        .context("write root report")?;

    let app = app_from_storage_root(storage_root.path(), 20, "UTC").await?;
    let body = get_search(app, "q=report").await?;
    let resources = search_resources(&body)?;

    assert_eq!(
        resources,
        vec![
            ("Report.txt", "alpha/Report.txt", "alpha"),
            ("report.txt", "alpha/nested/report.txt", "alpha/nested"),
            ("REPORT.txt", "beta/REPORT.txt", "beta"),
            ("report-notes.txt", "report-notes.txt", ""),
        ],
    );
    assert_eq!(body.get("truncated"), Some(&Value::Bool(false)));
    Ok(())
}

#[tokio::test]
async fn test_should_disambiguate_same_named_server_search_results_with_containing_paths()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir(storage_root.path().join("alpha"))
        .await
        .context("create alpha directory")?;
    fs::create_dir(storage_root.path().join("beta"))
        .await
        .context("create beta directory")?;
    fs::write(
        storage_root.path().join("alpha").join("report.txt"),
        b"alpha",
    )
    .await
    .context("write alpha report")?;
    fs::write(storage_root.path().join("beta").join("report.txt"), b"beta")
        .await
        .context("write beta report")?;

    let app = app_from_storage_root(storage_root.path(), 20, "UTC").await?;
    let body = get_search(app, "q=report").await?;

    assert_eq!(
        search_resources(&body)?,
        vec![
            ("report.txt", "alpha/report.txt", "alpha"),
            ("report.txt", "beta/report.txt", "beta"),
        ],
    );
    Ok(())
}

#[tokio::test]
async fn test_should_match_server_search_as_plain_resource_name_substring_only() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("literal-star-*.txt"), b"plain")
        .await
        .context("write literal wildcard resource")?;
    fs::write(storage_root.path().join("report.txt"), b"literal-star")
        .await
        .context("write content-only match")?;

    let app = app_from_storage_root(storage_root.path(), 20, "UTC").await?;
    let wildcard_body = get_search(app.clone(), "q=star-*").await?;
    assert_eq!(
        search_resources(&wildcard_body)?,
        vec![("literal-star-*.txt", "literal-star-*.txt", "")],
    );

    let content_body = get_search(app, "q=plain").await?;
    assert!(search_resources(&content_body)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_should_report_truncated_server_search_when_result_limit_is_reached() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("match-a.txt"), b"a")
        .await
        .context("write first match")?;
    fs::write(storage_root.path().join("match-b.txt"), b"b")
        .await
        .context("write second match")?;
    fs::write(storage_root.path().join("match-c.txt"), b"c")
        .await
        .context("write third match")?;

    let app =
        app_from_storage_root_with_search_limits(storage_root.path(), 10, 2, 100, "UTC").await?;
    let body = get_search(app, "q=match").await?;
    let resources = search_resources(&body)?;

    assert_eq!(
        resources,
        vec![
            ("match-a.txt", "match-a.txt", ""),
            ("match-b.txt", "match-b.txt", ""),
        ],
    );
    assert_eq!(body.get("truncated"), Some(&Value::Bool(true)));
    Ok(())
}

#[tokio::test]
async fn test_should_report_truncated_server_search_when_traversal_limit_is_reached() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::write(storage_root.path().join("target-a.txt"), b"a")
        .await
        .context("write first target")?;
    fs::write(storage_root.path().join("target-b.txt"), b"b")
        .await
        .context("write second target")?;

    let app =
        app_from_storage_root_with_search_limits(storage_root.path(), 10, 10, 1, "UTC").await?;
    let body = get_search(app, "q=target").await?;
    let resources = search_resources(&body)?;

    assert_eq!(resources, vec![("target-a.txt", "target-a.txt", "")]);
    assert_eq!(body.get("truncated"), Some(&Value::Bool(true)));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn test_should_exclude_reserved_staging_names_and_symlinks_from_server_search() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let outside = tempfile::tempdir().context("create outside directory")?;
    fs::write(storage_root.path().join("visible-target.txt"), b"visible")
        .await
        .context("write visible target")?;
    fs::create_dir(storage_root.path().join(".fh-staging"))
        .await
        .context("create reserved staging directory")?;
    fs::write(
        storage_root
            .path()
            .join(".fh-staging")
            .join("hidden-target.txt"),
        b"hidden",
    )
    .await
    .context("write hidden staging target")?;
    fs::write(outside.path().join("secret-target.txt"), b"secret")
        .await
        .context("write outside target")?;
    tokio::fs::symlink(
        outside.path().join("secret-target.txt"),
        storage_root.path().join("linked-target.txt"),
    )
    .await
    .context("create symlink target")?;

    let app = app_from_storage_root(storage_root.path(), 20, "UTC").await?;
    let body = get_search(app, "q=target").await?;
    let resources = search_resources(&body)?;

    assert_eq!(
        resources,
        vec![("visible-target.txt", "visible-target.txt", "")]
    );
    assert_eq!(body.get("truncated"), Some(&Value::Bool(false)));
    Ok(())
}

#[tokio::test]
async fn test_should_use_search_result_paths_for_file_download_directory_open_and_containing_path_navigation()
-> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    fs::create_dir_all(storage_root.path().join("docs").join("matches-dir"))
        .await
        .context("create matching directory")?;
    fs::write(
        storage_root
            .path()
            .join("docs")
            .join("matches-dir")
            .join("inside.txt"),
        b"inside",
    )
    .await
    .context("write file inside matching directory")?;
    fs::write(
        storage_root.path().join("docs").join("matches-file.txt"),
        b"download",
    )
    .await
    .context("write matching file")?;

    let app = app_from_storage_root(storage_root.path(), 20, "UTC").await?;
    let body = get_search(app.clone(), "q=matches").await?;
    let file_path = search_resource_path(&body, "matches-file.txt")?;
    let directory_path = search_resource_path(&body, "matches-dir")?;
    let containing_path = search_containing_path(&body, "matches-file.txt")?;

    let download = request_download(app.clone(), file_path).await?;
    assert_eq!(download.status(), StatusCode::OK);
    let download_body = to_bytes(download.into_body(), usize::MAX)
        .await
        .context("read search result file download")?;
    assert_eq!(download_body.as_ref(), b"download");

    let opened_directory = get_listing(app.clone(), Some(directory_path)).await?;
    assert_eq!(resource_names(&opened_directory)?, vec!["inside.txt"]);

    let containing_directory = get_listing(app, Some(containing_path)).await?;
    assert_eq!(
        resource_names(&containing_directory)?,
        vec!["matches-dir", "matches-file.txt"],
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
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
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

#[tokio::test]
async fn test_should_render_browser_file_resource_open_as_download_flow() -> Result<()> {
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

    assert!(body.contains("function downloadResource(resourcePath)"));
    assert!(body.contains("'/api/download?path=' + encodeURIComponent(resourcePath)"));
    assert!(body.contains("downloadResource(resource.resourcePath)"));
    Ok(())
}

#[tokio::test]
async fn test_should_render_browser_directory_archive_action_separate_from_directory_open()
-> Result<()> {
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

    assert!(body.contains("function downloadDirectoryArchive(resourcePath)"));
    assert!(body.contains("'/api/archive?path=' + encodeURIComponent(resourcePath)"));
    assert!(body.contains("downloadDirectoryArchive(resource.resourcePath)"));
    assert!(body.contains("Download archive"));
    Ok(())
}

#[tokio::test]
async fn test_should_render_browser_server_search_mode_switching_and_result_actions() -> Result<()>
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

    assert!(body.contains("id=\"search-mode\""));
    assert!(body.contains("value=\"currentListFilter\" selected"));
    assert!(body.contains("value=\"serverSearch\""));
    assert!(body.contains("id=\"server-search-submit\""));
    assert!(body.contains("serverSearchSubmit.hidden = state.searchMode !== 'serverSearch'"));
    assert!(body.contains("function runServerSearch()"));
    assert!(body.contains("fetch('/api/search?q=' + encodeURIComponent(state.filter))"));
    assert!(body.contains("function renderSearchRows(rows, truncated)"));
    assert!(body.contains("loadDirectory(row.containingPath)"));
    assert!(body.contains("downloadResource(row.resource.resourcePath)"));
    assert!(body.contains("loadDirectory(row.resource.resourcePath)"));
    assert!(body.contains("Search results truncated"));
    Ok(())
}

async fn app_from_storage_root(
    storage_root: &Path,
    listing_limit: usize,
    time_zone: &str,
) -> Result<axum::Router> {
    app_from_storage_root_with_archive_limits(
        storage_root,
        listing_limit,
        100,
        1_048_576,
        time_zone,
    )
    .await
}

async fn app_from_storage_root_with_archive_limits(
    storage_root: &Path,
    listing_limit: usize,
    archive_resource_count_limit: usize,
    archive_uncompressed_size_limit_bytes: u64,
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
  archive_resource_count_limit: {archive_resource_count_limit}
  archive_uncompressed_size_limit_bytes: {archive_uncompressed_size_limit_bytes}
  search_result_limit: 100
  search_traversal_limit: 1000
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

async fn app_from_storage_root_with_search_limits(
    storage_root: &Path,
    listing_limit: usize,
    search_result_limit: usize,
    search_traversal_limit: usize,
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
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: {search_result_limit}
  search_traversal_limit: {search_traversal_limit}
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

async fn get_search(app: axum::Router, query: &str) -> Result<Value> {
    let response = request_search(app, query).await?;

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read search response body")?;
    serde_json::from_slice(&body).context("parse search response body")
}

fn search_resources(body: &Value) -> Result<Vec<(&str, &str, &str)>> {
    let resources = body
        .get("resources")
        .and_then(Value::as_array)
        .context("search resources must be an array")?;

    resources
        .iter()
        .map(|row| {
            let resource = row.get("resource").context("search row resource exists")?;
            let name = resource
                .get("name")
                .and_then(Value::as_str)
                .context("search resource name must be a string")?;
            let resource_path = resource
                .get("resourcePath")
                .and_then(Value::as_str)
                .context("search resource path must be a string")?;
            let containing_path = row
                .get("containingPath")
                .and_then(Value::as_str)
                .context("search containing path must be a string")?;
            Ok((name, resource_path, containing_path))
        })
        .collect()
}

fn search_resource_path<'a>(body: &'a Value, name: &str) -> Result<&'a str> {
    let row = search_row_named(body, name)?;
    row.get("resource")
        .and_then(|resource| resource.get("resourcePath"))
        .and_then(Value::as_str)
        .with_context(|| format!("search resource path for {name} must be a string"))
}

fn search_containing_path<'a>(body: &'a Value, name: &str) -> Result<&'a str> {
    search_row_named(body, name)?
        .get("containingPath")
        .and_then(Value::as_str)
        .with_context(|| format!("search containing path for {name} must be a string"))
}

fn search_row_named<'a>(body: &'a Value, name: &str) -> Result<&'a Value> {
    let resources = body
        .get("resources")
        .and_then(Value::as_array)
        .context("search resources must be an array")?;
    resources
        .iter()
        .find(|row| {
            row.get("resource")
                .and_then(|resource| resource.get("name"))
                .and_then(Value::as_str)
                == Some(name)
        })
        .with_context(|| format!("search result {name} must exist"))
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

async fn request_download(app: axum::Router, path: &str) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri(format!("/api/download?path={path}"))
            .body(Body::empty())
            .context("build download request")?,
    )
    .await
    .context("send download request")
}

async fn request_download_without_path(app: axum::Router) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri("/api/download")
            .body(Body::empty())
            .context("build download request")?,
    )
    .await
    .context("send download request")
}

async fn request_archive(app: axum::Router, path: &str) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri(format!("/api/archive?path={path}"))
            .body(Body::empty())
            .context("build archive request")?,
    )
    .await
    .context("send archive request")
}

async fn request_search(app: axum::Router, query: &str) -> Result<axum::response::Response> {
    app.oneshot(
        Request::builder()
            .uri(format!("/api/search?{query}"))
            .body(Body::empty())
            .context("build search request")?,
    )
    .await
    .context("send search request")
}

fn zip_entry_names(bytes: &[u8]) -> Result<Vec<String>> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).context("open archive")?;
    let mut names = Vec::with_capacity(archive.len());
    for index in 0..archive.len() {
        let file = archive.by_index(index).context("read archive entry")?;
        names.push(file.name().to_owned());
    }
    Ok(names)
}

fn zip_entry_text(bytes: &[u8], name: &str) -> Result<String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(reader).context("open archive")?;
    let mut file = archive.by_name(name).context("find archive entry")?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .context("read archive entry text")?;
    Ok(text)
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
