use std::path::Path;

use anyhow::{Context, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use file_hub::{
    config::{AppConfig, AppConfigError},
    http::build_router,
};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn test_should_reject_zero_request_body_limit_from_yaml() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_directory = tempfile::tempdir().context("create temporary config directory")?;
    let config_path = config_directory.path().join("file-hub.yaml");
    tokio::fs::write(&config_path, config_yaml(storage_root.path(), 0, 8, 4))
        .await
        .context("write test configuration")?;

    let error = AppConfig::load_from_path(&config_path)
        .await
        .expect_err("zero request body limit must be rejected");

    assert!(matches!(error, AppConfigError::Validation(_)));
    Ok(())
}

#[tokio::test]
async fn test_should_return_unified_error_when_request_body_exceeds_configured_limit() -> Result<()>
{
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_directory = tempfile::tempdir().context("create temporary config directory")?;
    let config_path = config_directory.path().join("file-hub.yaml");
    tokio::fs::write(&config_path, config_yaml(storage_root.path(), 32, 8, 4))
        .await
        .context("write test configuration")?;
    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load test configuration")?;
    let app = build_router(config).await.context("build HTTP router")?;
    let request_body = serde_json::json!({
        "username": "admin",
        "password": "a password deliberately longer than the body limit",
    })
    .to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(request_body))
                .context("build oversized request")?,
        )
        .await
        .context("send oversized request")?;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("application/json")),
    );
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .context("read error response")?;
    let body: Value = serde_json::from_slice(&body).context("decode error response")?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&Value::String("request_body_too_large".to_owned()))
    );
    assert_eq!(
        body.pointer("/error/reason"),
        Some(&Value::String(
            "request body exceeds configured limit".to_owned()
        )),
    );
    assert!(
        !body
            .to_string()
            .contains(storage_root.path().to_string_lossy().as_ref())
    );
    Ok(())
}

#[tokio::test]
async fn test_should_reject_requests_above_configured_concurrency_limit() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_directory = tempfile::tempdir().context("create temporary config directory")?;
    let config_path = config_directory.path().join("file-hub.yaml");
    tokio::fs::write(&config_path, config_yaml(storage_root.path(), 4096, 1, 4))
        .await
        .context("write test configuration")?;
    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load test configuration")?;
    let app = build_router(config).await.context("build HTTP router")?;

    let requests = (0..16).map(|_| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"username":"admin","password":"wrong-password"}"#,
                    ))
                    .context("build concurrent request")?,
            )
            .await
            .context("send concurrent request")
        }
    });
    let responses = futures_util::future::join_all(requests).await;
    let mut limited_responses = Vec::new();
    for response in responses {
        let response = response?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            limited_responses.push(response);
        }
    }

    assert!(!limited_responses.is_empty());
    let body = to_bytes(
        limited_responses
            .pop()
            .context("at least one request should be concurrency limited")?
            .into_body(),
        usize::MAX,
    )
    .await
    .context("read concurrency error")?;
    let body: Value = serde_json::from_slice(&body).context("decode concurrency error")?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&Value::String(
            "request_concurrency_limit_exceeded".to_owned(),
        )),
    );
    Ok(())
}

#[tokio::test]
async fn test_should_serve_embedded_spa_and_static_assets_from_rust_router() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    let config_directory = tempfile::tempdir().context("create temporary config directory")?;
    let config_path = config_directory.path().join("file-hub.yaml");
    tokio::fs::write(&config_path, config_yaml(storage_root.path(), 4096, 8, 4))
        .await
        .context("write test configuration")?;
    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load test configuration")?;
    let app = build_router(config).await.context("build HTTP router")?;

    let root = app
        .clone()
        .oneshot(
            Request::get("/")
                .body(Body::empty())
                .context("build root request")?,
        )
        .await
        .context("request embedded index")?;
    assert_eq!(root.status(), StatusCode::OK);
    let root_body = to_bytes(root.into_body(), usize::MAX)
        .await
        .context("read embedded index")?;
    let root_body = String::from_utf8(root_body.to_vec()).context("index must be UTF-8")?;
    assert!(root_body.contains("<div id=\"app\"></div>"));
    assert!(root_body.contains("nomodule"));

    let fallback = app
        .clone()
        .oneshot(
            Request::get("/nested/browser/path")
                .body(Body::empty())
                .context("build SPA fallback request")?,
        )
        .await
        .context("request SPA fallback")?;
    assert_eq!(fallback.status(), StatusCode::OK);
    let fallback_body = to_bytes(fallback.into_body(), usize::MAX)
        .await
        .context("read SPA fallback")?;
    assert_eq!(fallback_body.as_ref(), root_body.as_bytes());

    let asset_path = first_asset_path(&root_body).context("index should reference an asset")?;
    let asset = app
        .oneshot(
            Request::get(asset_path)
                .body(Body::empty())
                .context("build asset request")?,
        )
        .await
        .context("request embedded asset")?;
    assert_eq!(asset.status(), StatusCode::OK);
    assert_ne!(
        asset.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "text/html; charset=utf-8"
        )),
    );
    Ok(())
}

fn first_asset_path(index: &str) -> Option<&str> {
    let marker = "\"/assets/";
    let start = index.find(marker)?.checked_add(1)?;
    let tail = index.get(start..)?;
    let end = tail.find('"')?;
    tail.get(..end)
}

#[tokio::test]
async fn test_should_reject_filesystem_requests_above_configured_concurrency_limit() -> Result<()> {
    let storage_root = tempfile::tempdir().context("create temporary storage root")?;
    for index in 0..500 {
        tokio::fs::write(
            storage_root.path().join(format!("file-{index:04}.txt")),
            b"content",
        )
        .await
        .context("write listing fixture")?;
    }
    let config_directory = tempfile::tempdir().context("create temporary config directory")?;
    let config_path = config_directory.path().join("file-hub.yaml");
    tokio::fs::write(&config_path, config_yaml(storage_root.path(), 4096, 32, 1))
        .await
        .context("write test configuration")?;
    let config = AppConfig::load_from_path(&config_path)
        .await
        .context("load test configuration")?;
    let app = build_router(config).await.context("build HTTP router")?;

    let responses = futures_util::future::join_all((0..16).map(|_| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::get("/api/list")
                    .body(Body::empty())
                    .context("build concurrent listing request")?,
            )
            .await
            .context("send concurrent listing request")
        }
    }))
    .await;
    let mut limited = None;
    for response in responses {
        let response = response?;
        if response.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(response);
            break;
        }
    }
    let body = to_bytes(
        limited
            .context("at least one filesystem request should be limited")?
            .into_body(),
        usize::MAX,
    )
    .await
    .context("read filesystem concurrency error")?;
    let body: Value =
        serde_json::from_slice(&body).context("decode filesystem concurrency error")?;
    assert_eq!(
        body.pointer("/error/code"),
        Some(&Value::String("fs_concurrency_limit_exceeded".to_owned())),
    );
    Ok(())
}

fn config_yaml(
    storage_root: &Path,
    request_body_limit_bytes: u64,
    request_concurrency_limit: usize,
    fs_concurrency_limit: usize,
) -> String {
    format!(
        r"storage_root: {storage_root}
server:
  bind_address: 127.0.0.1:0
  time_zone: UTC
limits:
  request_body_limit_bytes: {request_body_limit_bytes}
  request_concurrency_limit: {request_concurrency_limit}
  listing_direct_child_limit: 100
  archive_resource_count_limit: 100
  archive_uncompressed_size_limit_bytes: 1048576
  search_result_limit: 100
  search_traversal_limit: 1000
  request_timeout_seconds: 5
  fs_concurrency_limit: {fs_concurrency_limit}
",
        storage_root = storage_root.display(),
    )
}
