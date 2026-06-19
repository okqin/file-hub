# File Hub

File Hub is a single-process file browser. The production binary embeds the Vue SPA and serves it
with the JSON API; no separate frontend server or static directory is required at runtime.

## Build

Prerequisites are the pinned Rust toolchain and Bun 1.3 or newer.

```sh
make frontend-install
make package
```

The packaged executable is `target/release/file-hub`. Vite builds modern and legacy bundles for
Chrome/Chromium 69 and Firefox 59 before Rust embeds `frontend-dist` into that executable.

## Configure and run

Copy `file-hub.example.yaml` and set `storage_root` to an existing directory. All values under
`limits` are validated before the listener starts.

```sh
./target/release/file-hub ./file-hub.yaml
```

On the first startup, tracing output reports a generated bootstrap password for the fixed
Administrator username `admin`. Log in with it and change the password immediately. Until that
first password change succeeds, restarting File Hub generates and reports a new bootstrap
password. The password is not read from YAML.

The process serves the SPA, SPA fallback, and `/api/*` routes on `server.bind_address`. Application
diagnostics use `tracing`; product audit logging is intentionally out of scope.

## Verify

```sh
bun run test
bun run build
cargo build
cargo test
cargo +nightly fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --lib --bins --all-features -- -D warnings -W clippy::pedantic
cargo audit
cargo deny check
```

See [runtime and packaging](docs/runtime.md) for the configuration contract and packaged browser
smoke procedure.
