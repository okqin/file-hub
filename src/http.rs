//! HTTP routes for File Hub.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
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
    if query.path.as_deref().is_some_and(|path| !path.is_empty()) {
        return Err(ApiError::bad_request(
            "unsupported_resource_path",
            "this slice only supports Root Directory listing",
        ));
    }

    resource::list_root_directory(&state.config)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

impl ApiError {
    fn bad_request(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            reason: reason.into(),
        }
    }
}

impl From<resource::ResourceError> for ApiError {
    fn from(error: resource::ResourceError) -> Self {
        match error {
            resource::ResourceError::ListingLimitExceeded { limit } => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "listing_limit_exceeded",
                reason: format!("direct child listing exceeds configured limit of {limit}"),
            },
            resource::ResourceError::InvalidResourceName => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "invalid_resource_name",
                reason: "resource name is not valid UTF-8".to_owned(),
            },
            resource::ResourceError::ReadRoot(_)
            | resource::ResourceError::ReadEntry(_)
            | resource::ResourceError::Metadata(_)
            | resource::ResourceError::ModifiedTime(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "resource_listing_failed",
                reason: "failed to list Root Directory".to_owned(),
            },
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
    <section aria-label="Root Directory">
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Kind</th>
            <th>Size</th>
            <th>Modified</th>
          </tr>
        </thead>
        <tbody id="resources">
          <tr><td colspan="4" class="muted">Loading</td></tr>
        </tbody>
      </table>
    </section>
  </main>
  <script>
    (function () {
      var resources = document.getElementById('resources');

      function text(value) {
        return document.createTextNode(value == null ? '' : String(value));
      }

      function renderRows(rows) {
        resources.textContent = '';
        if (!rows.length) {
          var emptyRow = document.createElement('tr');
          var emptyCell = document.createElement('td');
          emptyCell.colSpan = 4;
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
          name.appendChild(text(resource.name));
          var kind = document.createElement('td');
          kind.appendChild(text(resource.kind));
          var size = document.createElement('td');
          size.appendChild(text(resource.size));
          var modified = document.createElement('td');
          modified.appendChild(text(resource.modifiedTime));
          row.appendChild(name);
          row.appendChild(kind);
          row.appendChild(size);
          row.appendChild(modified);
          resources.appendChild(row);
        });
      }

      function renderError(message) {
        resources.textContent = '';
        var row = document.createElement('tr');
        var cell = document.createElement('td');
        cell.colSpan = 4;
        cell.className = 'error';
        cell.appendChild(text(message));
        row.appendChild(cell);
        resources.appendChild(row);
      }

      fetch('/api/list')
        .then(function (response) {
          return response.json().then(function (body) {
            if (!response.ok) {
              throw new Error(body.error && body.error.reason ? body.error.reason : 'Listing failed');
            }
            return body;
          });
        })
        .then(function (body) {
          renderRows(body.resources || []);
        })
        .catch(function (error) {
          renderError(error.message);
        });
    })();
  </script>
</body>
</html>
"#;
