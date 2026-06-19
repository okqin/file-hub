# Runtime and packaging

## Delivery shape

`make package` runs the Vite production build and then compiles `target/release/file-hub`.
`rust-embed` stores every file from `frontend-dist` in the executable, including the modern and
legacy JavaScript bundles. The Rust process serves embedded assets, non-API SPA fallback, and the
API from one listener. Paths below `/api/` never fall back to HTML.

`frontend-dist` is committed so a Rust-only build still has deterministic embed input. Any frontend
change must regenerate it with `bun run build` and include the resulting hashed files.

## Configuration

File Hub accepts one positional argument: a YAML configuration path. Unknown fields and values
outside their accepted ranges stop startup. See [`file-hub.example.yaml`](../file-hub.example.yaml)
for every supported field.

- `request_body_limit_bytes` caps bodies before API extraction.
- `request_concurrency_limit` rejects excess in-flight requests with HTTP 429.
- `request_timeout_seconds` aborts requests that exceed the configured duration.
- Upload, archive, listing, and search limits are enforced by their corresponding API operations.
- `fs_concurrency_limit` bounds the configured filesystem concurrency budget.

Framework-level body, concurrency, timeout, and missing-API failures use the same JSON error
envelope as domain failures. Responses do not include the storage root, database path, or secrets.

## Administrator bootstrap

The first startup creates the fixed `admin` identity and emits its generated bootstrap password as
a warning through `tracing`. The bootstrap password remains active only until the Administrator
successfully changes it. File Hub never reads an Administrator password from configuration.

## Packaged browser smoke

1. Create a temporary storage root containing a directory and a readable file.
2. Start `target/release/file-hub` with a temporary YAML configuration and capture stderr.
3. Open the configured address and confirm the embedded UI lists both resources.
4. Log in as `admin` with the captured bootstrap password.
5. Open a directory or download the readable file through the UI.
6. Stop the process and remove the temporary storage root and database.
