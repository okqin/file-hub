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
    sort: Option<resource::SortField>,
    order: Option<resource::SortOrder>,
    filter: Option<String>,
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
            resource::ResourceError::ReadDirectory(_)
            | resource::ResourceError::ReadEntry(_)
            | resource::ResourceError::Metadata(_)
            | resource::ResourceError::ModifiedTime(_) => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "resource_listing_failed",
                reason: "failed to list Directory".to_owned(),
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
          if (resource.kind === 'directory') {
            var open = document.createElement('button');
            open.type = 'button';
            open.appendChild(text(resource.name));
            open.addEventListener('click', function () {
              loadDirectory(resource.resourcePath);
            });
            name.appendChild(open);
          } else {
            name.appendChild(text(resource.name));
          }
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
