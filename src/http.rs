//! HTTP routes for File Hub.

use std::sync::Arc;

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use tokio_util::io::ReaderStream;
use tower::ServiceBuilder;
use tower_http::timeout::TimeoutLayer;

use crate::{config::AppConfig, resource};

/// Build the public HTTP router.
pub fn build_router(config: AppConfig) -> Router {
    let timeout = std::time::Duration::from_secs(
        config
            .limits()
            .request_timeout_seconds()
            .get()
            .try_into()
            .map_or(300, |value| value),
    );
    let state = AppState {
        config: Arc::new(config),
    };

    Router::new()
        .route("/", get(index))
        .route("/api/list", get(list_root_directory))
        .route("/api/download", get(download_file))
        .route("/api/archive", get(download_directory_archive))
        .with_state(state)
        .layer(ServiceBuilder::new().layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            timeout,
        )))
}

#[derive(Clone, Debug)]
struct AppState {
    config: Arc<AppConfig>,
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

impl ApiError {
    fn internal_server_error() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_server_error",
            reason: "internal server error".to_owned(),
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
    input {
      min-width: 260px;
      padding: 8px 10px;
      border: 1px solid #d8dee4;
      border-radius: 6px;
      font: inherit;
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
      <div class="identity">Anonymous</div>
    </header>
    <nav aria-label="Breadcrumb" class="breadcrumb" id="breadcrumb"></nav>
    <div class="toolbar">
      <button type="button" id="return-to-parent" class="parent" hidden>Return to parent</button>
      <div class="filter">
        <label for="current-list-filter">Current List Filter</label>
        <input id="current-list-filter" type="search" autocomplete="off">
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
      var filter = document.getElementById('current-list-filter');
      var sortButtons = Array.prototype.slice.call(document.querySelectorAll('[data-sort-field]'));
      var sortIndicators = Array.prototype.slice.call(document.querySelectorAll('[data-sort-indicator]'));
      var state = {
        path: '',
        sort: 'name',
        order: 'asc',
        filter: ''
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
            renderSort(body.sort || { field: state.sort, order: state.order });
            renderRows(body.resources || []);
          })
          .catch(function (error) {
            renderError(error.message);
          });
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

      function renderSort(sort) {
        state.sort = sort.field || state.sort;
        state.order = sort.order || state.order;
        sortIndicators.forEach(function (indicator) {
          var field = indicator.getAttribute('data-sort-indicator');
          indicator.textContent = field === state.sort ? state.order : '';
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

      filter.addEventListener('input', function () {
        state.filter = filter.value;
        loadDirectory(state.path);
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

      loadDirectory('');
    }());
  </script>
</body>
</html>
"#;
