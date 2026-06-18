//! YAML configuration loading and validation.

use std::{
    net::SocketAddr,
    num::{NonZeroU64, NonZeroUsize},
    path::{Path, PathBuf},
};

use chrono_tz::Tz;
use config::{Config, File, FileFormat};
use serde::Deserialize;
use thiserror::Error;
use tokio::fs;
use validator::{Validate, ValidationError, ValidationErrors};

/// Validated application configuration.
#[derive(Clone, Debug)]
pub struct AppConfig {
    storage_root: PathBuf,
    database_path: PathBuf,
    staging_directory_name: String,
    server: ServerConfig,
    limits: RuntimeLimits,
}

/// Runtime server settings.
#[derive(Clone, Copy, Debug)]
pub struct ServerConfig {
    bind_address: SocketAddr,
    time_zone: Tz,
}

/// Bounded runtime settings.
#[derive(Clone, Copy, Debug)]
pub struct RuntimeLimits {
    upload_single_file_size_limit_bytes: NonZeroU64,
    upload_total_size_limit_bytes: NonZeroU64,
    directory_upload_resource_count_limit: NonZeroUsize,
    listing_direct_child_limit: NonZeroUsize,
    archive_resource_count_limit: NonZeroUsize,
    archive_uncompressed_size_limit_bytes: NonZeroU64,
    search_result_limit: NonZeroUsize,
    search_traversal_limit: NonZeroUsize,
    request_timeout_seconds: NonZeroUsize,
    fs_concurrency_limit: NonZeroUsize,
}

/// Application configuration loading and validation errors.
#[derive(Debug, Error)]
pub enum AppConfigError {
    /// The configuration source could not be read or decoded.
    #[error("configuration source is invalid")]
    Source(#[from] config::ConfigError),
    /// The configuration shape failed validation.
    #[error("configuration values are invalid")]
    Validation(#[from] ValidationErrors),
    /// The configured socket address is invalid.
    #[error("server bind address is invalid")]
    BindAddress(#[source] std::net::AddrParseError),
    /// The configured time zone is invalid.
    #[error("server time zone is invalid")]
    TimeZone(#[source] chrono_tz::ParseError),
    /// A numeric limit that must be non-zero was zero.
    #[error("{field} must be greater than zero")]
    ZeroLimit {
        /// The invalid field name.
        field: &'static str,
    },
    /// The configured storage root does not exist or is not a directory.
    #[error("storage root must be an existing directory")]
    StorageRoot(#[source] std::io::Error),
    /// The configured storage root exists but is not a directory.
    #[error("storage root must be a directory")]
    StorageRootNotDirectory,
    /// The staging directory name is not a safe resource name.
    #[error("staging directory name is invalid")]
    InvalidStagingDirectoryName,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppConfig {
    storage_root: PathBuf,
    database_path: Option<PathBuf>,
    #[serde(default = "default_staging_directory_name")]
    staging_directory_name: String,
    server: RawServerConfig,
    limits: RawRuntimeLimits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawServerConfig {
    bind_address: String,
    time_zone: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeLimits {
    #[serde(default = "default_upload_single_file_size_limit_bytes")]
    upload_single_file_size_limit_bytes: u64,
    #[serde(default = "default_upload_total_size_limit_bytes")]
    upload_total_size_limit_bytes: u64,
    #[serde(default = "default_directory_upload_resource_count_limit")]
    directory_upload_resource_count_limit: usize,
    listing_direct_child_limit: usize,
    archive_resource_count_limit: usize,
    archive_uncompressed_size_limit_bytes: u64,
    search_result_limit: usize,
    search_traversal_limit: usize,
    request_timeout_seconds: usize,
    fs_concurrency_limit: usize,
}

impl Validate for RawAppConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        add_length_error(
            &mut errors,
            "staging_directory_name",
            &self.staging_directory_name,
            1,
            255,
        );
        errors.merge_self("server", self.server.validate());
        errors.merge_self("limits", self.limits.validate());
        finish_validation(errors)
    }
}

impl Validate for RawServerConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        add_length_error(&mut errors, "bind_address", &self.bind_address, 1, 128);
        add_length_error(&mut errors, "time_zone", &self.time_zone, 1, 128);
        finish_validation(errors)
    }
}

impl Validate for RawRuntimeLimits {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        add_u64_range_error(
            &mut errors,
            "upload_single_file_size_limit_bytes",
            self.upload_single_file_size_limit_bytes,
            1,
            1_099_511_627_776,
        );
        add_u64_range_error(
            &mut errors,
            "upload_total_size_limit_bytes",
            self.upload_total_size_limit_bytes,
            1,
            1_099_511_627_776,
        );
        add_range_error(
            &mut errors,
            "directory_upload_resource_count_limit",
            self.directory_upload_resource_count_limit,
            1,
            1_000_000,
        );
        add_range_error(
            &mut errors,
            "listing_direct_child_limit",
            self.listing_direct_child_limit,
            1,
            100_000,
        );
        add_range_error(
            &mut errors,
            "archive_resource_count_limit",
            self.archive_resource_count_limit,
            1,
            1_000_000,
        );
        add_u64_range_error(
            &mut errors,
            "archive_uncompressed_size_limit_bytes",
            self.archive_uncompressed_size_limit_bytes,
            1,
            1_099_511_627_776,
        );
        add_range_error(
            &mut errors,
            "search_result_limit",
            self.search_result_limit,
            1,
            100_000,
        );
        add_range_error(
            &mut errors,
            "search_traversal_limit",
            self.search_traversal_limit,
            1,
            1_000_000,
        );
        add_range_error(
            &mut errors,
            "request_timeout_seconds",
            self.request_timeout_seconds,
            1,
            300,
        );
        add_range_error(
            &mut errors,
            "fs_concurrency_limit",
            self.fs_concurrency_limit,
            1,
            4_096,
        );
        finish_validation(errors)
    }
}

impl AppConfig {
    /// Load and validate application configuration from a YAML file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unreadable, malformed, semantically invalid, or
    /// references a storage root that is not an existing directory.
    pub async fn load_from_path(path: impl AsRef<Path>) -> Result<Self, AppConfigError> {
        let path = path.as_ref().to_string_lossy();
        let raw: RawAppConfig = Config::builder()
            .add_source(File::new(path.as_ref(), FileFormat::Yaml))
            .build()?
            .try_deserialize()?;
        raw.validate()?;

        let storage_root = fs::canonicalize(&raw.storage_root)
            .await
            .map_err(AppConfigError::StorageRoot)?;
        let metadata = fs::metadata(&storage_root)
            .await
            .map_err(AppConfigError::StorageRoot)?;
        if !metadata.is_dir() {
            return Err(AppConfigError::StorageRootNotDirectory);
        }

        if !is_valid_resource_name(&raw.staging_directory_name) {
            return Err(AppConfigError::InvalidStagingDirectoryName);
        }

        let server = ServerConfig {
            bind_address: raw
                .server
                .bind_address
                .parse()
                .map_err(AppConfigError::BindAddress)?,
            time_zone: raw
                .server
                .time_zone
                .parse()
                .map_err(AppConfigError::TimeZone)?,
        };
        let limits = RuntimeLimits {
            upload_single_file_size_limit_bytes: non_zero_u64(
                raw.limits.upload_single_file_size_limit_bytes,
                "limits.upload_single_file_size_limit_bytes",
            )?,
            upload_total_size_limit_bytes: non_zero_u64(
                raw.limits.upload_total_size_limit_bytes,
                "limits.upload_total_size_limit_bytes",
            )?,
            directory_upload_resource_count_limit: non_zero(
                raw.limits.directory_upload_resource_count_limit,
                "limits.directory_upload_resource_count_limit",
            )?,
            listing_direct_child_limit: non_zero(
                raw.limits.listing_direct_child_limit,
                "limits.listing_direct_child_limit",
            )?,
            archive_resource_count_limit: non_zero(
                raw.limits.archive_resource_count_limit,
                "limits.archive_resource_count_limit",
            )?,
            archive_uncompressed_size_limit_bytes: non_zero_u64(
                raw.limits.archive_uncompressed_size_limit_bytes,
                "limits.archive_uncompressed_size_limit_bytes",
            )?,
            search_result_limit: non_zero(
                raw.limits.search_result_limit,
                "limits.search_result_limit",
            )?,
            search_traversal_limit: non_zero(
                raw.limits.search_traversal_limit,
                "limits.search_traversal_limit",
            )?,
            request_timeout_seconds: non_zero(
                raw.limits.request_timeout_seconds,
                "limits.request_timeout_seconds",
            )?,
            fs_concurrency_limit: non_zero(
                raw.limits.fs_concurrency_limit,
                "limits.fs_concurrency_limit",
            )?,
        };

        Ok(Self {
            database_path: raw.database_path.unwrap_or_else(|| {
                storage_root
                    .join(&raw.staging_directory_name)
                    .join("file-hub.sqlite")
            }),
            storage_root,
            staging_directory_name: raw.staging_directory_name,
            server,
            limits,
        })
    }

    /// Return the canonical storage root.
    #[must_use]
    pub fn storage_root(&self) -> &Path {
        &self.storage_root
    }

    /// Return the `SQLite` database path used for authentication and sessions.
    #[must_use]
    pub fn database_path(&self) -> &Path {
        &self.database_path
    }

    /// Return the reserved staging directory name filtered from resource listings.
    #[must_use]
    pub fn staging_directory_name(&self) -> &str {
        &self.staging_directory_name
    }

    /// Return server settings.
    #[must_use]
    pub const fn server(&self) -> ServerConfig {
        self.server
    }

    /// Return runtime limits.
    #[must_use]
    pub const fn limits(&self) -> RuntimeLimits {
        self.limits
    }
}

impl ServerConfig {
    /// Return the socket address the HTTP server should bind.
    #[must_use]
    pub const fn bind_address(self) -> SocketAddr {
        self.bind_address
    }

    /// Return the configured display time zone.
    #[must_use]
    pub const fn time_zone(self) -> Tz {
        self.time_zone
    }
}

impl RuntimeLimits {
    /// Return the maximum byte size of one uploaded File.
    #[must_use]
    pub const fn upload_single_file_size_limit_bytes(self) -> NonZeroU64 {
        self.upload_single_file_size_limit_bytes
    }

    /// Return the maximum aggregate File content byte size of one upload request.
    #[must_use]
    pub const fn upload_total_size_limit_bytes(self) -> NonZeroU64 {
        self.upload_total_size_limit_bytes
    }

    /// Return the maximum number of Resources in one Directory Upload.
    #[must_use]
    pub const fn directory_upload_resource_count_limit(self) -> NonZeroUsize {
        self.directory_upload_resource_count_limit
    }

    /// Return the maximum number of direct child resources in one listing.
    #[must_use]
    pub const fn listing_direct_child_limit(self) -> NonZeroUsize {
        self.listing_direct_child_limit
    }

    /// Return the maximum resource count in one Directory Archive.
    #[must_use]
    pub const fn archive_resource_count_limit(self) -> NonZeroUsize {
        self.archive_resource_count_limit
    }

    /// Return the maximum uncompressed byte size in one Directory Archive.
    #[must_use]
    pub const fn archive_uncompressed_size_limit_bytes(self) -> NonZeroU64 {
        self.archive_uncompressed_size_limit_bytes
    }

    /// Return the maximum number of Server Search results returned in one response.
    #[must_use]
    pub const fn search_result_limit(self) -> NonZeroUsize {
        self.search_result_limit
    }

    /// Return the maximum number of Resources traversed by one Server Search.
    #[must_use]
    pub const fn search_traversal_limit(self) -> NonZeroUsize {
        self.search_traversal_limit
    }

    /// Return the request timeout in seconds.
    #[must_use]
    pub const fn request_timeout_seconds(self) -> NonZeroUsize {
        self.request_timeout_seconds
    }

    /// Return the maximum number of concurrent filesystem operations.
    #[must_use]
    pub const fn fs_concurrency_limit(self) -> NonZeroUsize {
        self.fs_concurrency_limit
    }
}

fn default_staging_directory_name() -> String {
    ".fh-staging".to_owned()
}

const fn default_upload_single_file_size_limit_bytes() -> u64 {
    10 * 1024 * 1024
}

const fn default_upload_total_size_limit_bytes() -> u64 {
    100 * 1024 * 1024
}

const fn default_directory_upload_resource_count_limit() -> usize {
    10_000
}

fn non_zero(value: usize, field: &'static str) -> Result<NonZeroUsize, AppConfigError> {
    NonZeroUsize::new(value).ok_or(AppConfigError::ZeroLimit { field })
}

fn non_zero_u64(value: u64, field: &'static str) -> Result<NonZeroU64, AppConfigError> {
    NonZeroU64::new(value).ok_or(AppConfigError::ZeroLimit { field })
}

fn is_valid_resource_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && !name.chars().any(char::is_control)
}

fn add_length_error(
    errors: &mut ValidationErrors,
    field: &'static str,
    value: &str,
    min: usize,
    max: usize,
) {
    let length = value.len();
    if length < min || length > max {
        errors.add(field, ValidationError::new("length"));
    }
}

fn add_range_error(
    errors: &mut ValidationErrors,
    field: &'static str,
    value: usize,
    min: usize,
    max: usize,
) {
    if value < min || value > max {
        errors.add(field, ValidationError::new("range"));
    }
}

fn add_u64_range_error(
    errors: &mut ValidationErrors,
    field: &'static str,
    value: u64,
    min: u64,
    max: u64,
) {
    if value < min || value > max {
        errors.add(field, ValidationError::new("range"));
    }
}

fn finish_validation(errors: ValidationErrors) -> Result<(), ValidationErrors> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
